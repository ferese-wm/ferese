//! Private protocol clients and offscreen EGL; no host desktop or DRM required.
use std::cell::RefCell;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay::backend::allocator::Fourcc;
use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::{Bind, ExportMem, Offscreen};
use smithay::desktop::Window;
use smithay::output::{Mode, Output, PhysicalProperties, Scale, Subpixel};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_wm_base::XdgWmBase;
use smithay::reexports::wayland_server::protocol::wl_compositor::WlCompositor;
use smithay::reexports::wayland_server::protocol::wl_shm::WlShm;
use smithay::reexports::wayland_server::{Display, Resource};
use smithay::utils::{Rectangle, Transform};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::{XdgShellHandler, XdgToplevelSurfaceData, XdgWmBaseUserData};

use crate::Ferese;
use crate::state::{ClientState, DesktopOutput};

pub(crate) fn request(
    wire: &mut UnixStream,
    object: u32,
    opcode: u32,
    args: &[u32],
    fd: Option<RawFd>,
) {
    let bytes = [object, (((args.len() + 2) * 4) as u32) << 16 | opcode]
        .into_iter()
        .chain(args.iter().copied())
        .flat_map(u32::to_ne_bytes)
        .collect::<Vec<_>>();
    if let Some(fd) = fd {
        let mut iov = libc::iovec {
            iov_base: bytes.as_ptr().cast_mut().cast(),
            iov_len: bytes.len(),
        };
        let mut control = [0usize; 8];
        // SAFETY: aligned control storage and iovec remain live for sendmsg;
        // the one SCM_RIGHTS entry contains an owned, open tempfile descriptor.
        unsafe {
            let mut msg: libc::msghdr = std::mem::zeroed();
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as usize;
            let header = libc::CMSG_FIRSTHDR(&msg);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as usize;
            libc::CMSG_DATA(header).cast::<RawFd>().write(fd);
            assert_eq!(
                libc::sendmsg(wire.as_raw_fd(), &msg, 0),
                bytes.len() as isize
            );
        }
    } else {
        wire.write_all(&bytes).unwrap();
    }
}

pub(crate) fn dispatch(events: &mut EventLoop<'static, Ferese>, state: &mut Ferese) {
    events.dispatch(Duration::from_millis(1), state).unwrap();
    state.display_handle.flush_clients().unwrap();
}

pub(crate) fn ack_configure(wire: &mut UnixStream, xdg: u32) -> u32 {
    ack_configure_with_size(wire, xdg, None).0
}

fn ack_configure_with_size(
    wire: &mut UnixStream,
    xdg: u32,
    toplevel: Option<u32>,
) -> (u32, Option<(u32, u32)>) {
    wire.set_nonblocking(true).unwrap();
    let mut bytes = Vec::new();
    let _ = wire.read_to_end(&mut bytes);
    wire.set_nonblocking(false).unwrap();
    let mut serial = None;
    let mut size = None;
    let mut offset = 0;
    while offset < bytes.len() {
        let object = u32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let header = u32::from_ne_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        if Some(object) == toplevel && header & 0xffff == 0 {
            let width = u32::from_ne_bytes(bytes[offset + 8..offset + 12].try_into().unwrap());
            let height = u32::from_ne_bytes(bytes[offset + 12..offset + 16].try_into().unwrap());
            size = Some((width, height));
        }
        if object == xdg && header & 0xffff == 0 {
            serial = Some(u32::from_ne_bytes(
                bytes[offset + 8..offset + 12].try_into().unwrap(),
            ));
        }
        offset += (header >> 16) as usize;
    }
    assert!(serial.is_some(), "missing configure for {xdg}: {bytes:?}");
    request(wire, xdg, 4, &[serial.unwrap()], None);
    (serial.unwrap(), size)
}

pub(crate) fn window(
    state: &mut Ferese,
    events: &mut EventLoop<'static, Ferese>,
    color: u32,
) -> (Window, UnixStream) {
    window_with_app_id(state, events, color, "")
}

pub(crate) fn window_with_app_id(
    state: &mut Ferese,
    events: &mut EventLoop<'static, Ferese>,
    color: u32,
    app_id: &str,
) -> (Window, UnixStream) {
    let (server, mut wire) = UnixStream::pair().unwrap();
    let client = state
        .display_handle
        .insert_client(server, Arc::new(ClientState::default()))
        .unwrap();
    let compositor = client
        .create_resource::<WlCompositor, smithay::wayland::GlobalData, Ferese>(
            &state.display_handle,
            6,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    let shell = client
        .create_resource::<XdgWmBase, XdgWmBaseUserData, Ferese>(
            &state.display_handle,
            6,
            Default::default(),
        )
        .unwrap();
    let shm = client
        .create_resource::<WlShm, smithay::wayland::GlobalData, Ferese>(
            &state.display_handle,
            1,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    request(&mut wire, compositor.id().protocol_id(), 0, &[2], None);
    request(&mut wire, shell.id().protocol_id(), 2, &[3, 2], None);
    request(&mut wire, 3, 1, &[4], None);
    if !app_id.is_empty() {
        let mut bytes = app_id.as_bytes().to_vec();
        bytes.push(0);
        let mut args = vec![bytes.len() as u32];
        bytes.resize(bytes.len().next_multiple_of(4), 0);
        args.extend(
            bytes
                .chunks_exact(4)
                .map(|word| u32::from_ne_bytes(word.try_into().unwrap())),
        );
        request(&mut wire, 4, 3, &args, None);
    }
    request(&mut wire, 2, 6, &[], None);
    dispatch(events, state);
    let (_, configured) = ack_configure_with_size(&mut wire, 3, Some(4));
    // Application fixtures honor the first configured size, as Settings does.
    // Existing capture fixtures keep their fixed 64x48 buffer.
    let (width, height) = if app_id.is_empty() {
        (64, 48)
    } else {
        let (width, height) = configured.expect("initial toplevel configure");
        (
            if width > 0 { width } else { 64 },
            if height > 0 { height } else { 48 },
        )
    };
    let buffer_size = width * height * 4;
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&color.to_ne_bytes().repeat((width * height) as usize))
        .unwrap();
    request(
        &mut wire,
        shm.id().protocol_id(),
        0,
        &[5, buffer_size],
        Some(file.as_raw_fd()),
    );
    request(&mut wire, 5, 0, &[6, 0, width, height, width * 4, 0], None);
    request(&mut wire, 3, 3, &[0, 0, width, height], None);
    request(&mut wire, 2, 1, &[6, 0, 0], None);
    request(&mut wire, 2, 2, &[0, 0, width, height], None);
    request(&mut wire, 2, 6, &[], None);
    dispatch(events, state);
    let window = state
        .windows
        .ids()
        .keys()
        .find(|window| {
            window
                .toplevel()
                .unwrap()
                .wl_surface()
                .client()
                .is_some_and(|owner| owner.id() == client.id())
        })
        .expect("mapped window")
        .clone();
    (window, wire)
}

fn attach_children(
    state: &mut Ferese,
    events: &mut EventLoop<'static, Ferese>,
    window: &Window,
    wire: &mut UnixStream,
) {
    use smithay::reexports::wayland_server::protocol::wl_subcompositor::WlSubcompositor;
    let client = window.toplevel().unwrap().wl_surface().client().unwrap();
    let compositor = client
        .create_resource::<WlCompositor, smithay::wayland::GlobalData, Ferese>(
            &state.display_handle,
            6,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    let subcompositor = client
        .create_resource::<WlSubcompositor, smithay::wayland::GlobalData, Ferese>(
            &state.display_handle,
            1,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    request(wire, compositor.id().protocol_id(), 0, &[7], None);
    request(wire, subcompositor.id().protocol_id(), 1, &[8, 7, 2], None);
    request(wire, 8, 1, &[11, 11], None);
    request(wire, 7, 1, &[6, 0, 0], None);
    request(wire, 7, 6, &[], None);
    request(wire, 2, 6, &[], None);
    let shell = client
        .create_resource::<XdgWmBase, XdgWmBaseUserData, Ferese>(
            &state.display_handle,
            6,
            Default::default(),
        )
        .unwrap();
    request(wire, compositor.id().protocol_id(), 0, &[9], None);
    request(wire, shell.id().protocol_id(), 2, &[10, 9], None);
    request(wire, shell.id().protocol_id(), 1, &[11], None);
    request(wire, 11, 1, &[32, 16], None);
    request(wire, 11, 2, &[32, 16, 1, 1], None);
    request(wire, 10, 2, &[12, 3, 11], None);
    request(wire, 9, 6, &[], None);
    dispatch(events, state);
    ack_configure(wire, 10);
    request(wire, 10, 3, &[0, 0, 32, 16], None);
    request(wire, 9, 1, &[6, 0, 0], None);
    request(wire, 9, 6, &[], None);
    dispatch(events, state);
    assert_eq!(
        smithay::desktop::PopupManager::popups_for_surface(window.toplevel().unwrap().wl_surface())
            .count(),
        1
    );
}

fn app_id(state: &mut Ferese, window: &Window, app: &str) {
    let top = window.toplevel().unwrap();
    with_states(top.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .unwrap()
            .lock()
            .unwrap()
            .app_id = Some(app.into());
    });
    state.app_id_changed(top.clone());
}

struct OutputReadback {
    request: u64,
    parts: calloop::channel::Channel<crate::handlers::screenshot::PartOutcome>,
    _reply: std::sync::mpsc::Receiver<ferese_ipc::Response>,
}

impl OutputReadback {
    fn queue(
        state: &mut Ferese,
        output: &Output,
        region: Rectangle<i32, smithay::utils::Buffer>,
    ) -> Self {
        let (reply, replies) = std::sync::mpsc::sync_channel(1);
        let scale = output.current_scale().fractional_scale();
        let request = state
            .screenshot
            .admit(
                1,
                1,
                reply,
                vec![crate::handlers::screenshot::PartSpec {
                    preserve_alpha: false,
                    premultiplied_rgba: false,
                    transform: output.current_transform(),
                    scale,
                    location: (0, 0),
                    logical_width: (f64::from(region.size.w) / scale).round() as i32,
                    logical_height: (f64::from(region.size.h) / scale).round() as i32,
                    buffer_width: region.size.w,
                    buffer_height: region.size.h,
                }],
            )
            .unwrap();
        let (sender, parts) = calloop::channel::channel();
        let permit = state.screenshot.permit(request);
        state.pending_screencopies.push(
            crate::handlers::screencopy::PendingScreencopy::owned(
                request,
                0,
                sender,
                output.clone(),
                region,
            )
            .with_permit(permit),
        );
        Self {
            request,
            parts,
            _reply: replies,
        }
    }

    fn wait(
        self,
        events: &mut EventLoop<'static, Ferese>,
        state: &mut Ferese,
    ) -> crate::handlers::screenshot::CaptureBuffer {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.parts.try_recv() {
                Ok(part) => {
                    assert_eq!(part.request, self.request);
                    state
                        .screenshot
                        .reject(self.request, "fixture consumed the readback");
                    return part.result.expect("the admitted GPU readback must succeed");
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    assert!(
                        Instant::now() < deadline,
                        "GPU fixture readback did not complete within five seconds"
                    );
                    // The fixture's external renderer borrow has ended. Its
                    // registered owner is now available to the real completion.
                    events.dispatch(Duration::from_millis(2), state).unwrap();
                    state.display_handle.flush_clients().unwrap();
                }
                Err(error) => panic!("GPU fixture completion channel closed: {error}"),
            }
        }
    }
}

fn pixels(
    state: &mut Ferese,
    renderer: &Rc<RefCell<GlesRenderer>>,
    events: &mut EventLoop<'static, Ferese>,
    output: &Output,
    capture: bool,
    texture: &mut GlesTexture,
) -> Vec<u8> {
    let scene = state.sample_frame(output, Duration::ZERO);
    if capture {
        let pending = OutputReadback::queue(state, output, Rectangle::from_size((320, 240).into()));
        let mut renderer = renderer.borrow_mut();
        // Retain damage history as well as pixels: changing privacy policy
        // must erase content already present in the reused capture target.
        let mut target = std::mem::take(&mut state.nested_capture_texture);
        target[0].get_or_insert_with(|| {
            crate::backends::direct::capture::CaptureTarget::new(texture.clone(), output)
        });
        crate::backends::direct::capture::capture_output(
            state,
            &mut renderer,
            &mut target,
            output,
            &scene,
        );
        state.nested_capture_texture = target;
        drop(renderer);
        let mut result = pending.wait(events, state).pixels;
        for pixel in result.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        return result;
    }
    let mut renderer = renderer.borrow_mut();
    let elements =
        crate::render::sampled_output_elements(state, &mut renderer, output, false, &scene);
    let mut target = renderer.bind(texture).unwrap();
    crate::render::redraw_output(&mut *renderer, &mut target, output, &elements).unwrap();
    let mapping = renderer
        .copy_framebuffer(
            &target,
            Rectangle::from_size((320, 240).into()),
            Fourcc::Abgr8888,
        )
        .unwrap();
    renderer.map_texture(&mapping).unwrap().to_vec()
}

#[test]
#[ignore = "requires private Wayland sockets and an offscreen EGL device"]
fn lock_scene_draws_the_pointer_and_delivers_client_cursor_frames() {
    if !crate::startup_tests::private_runtime(
        "state::capture_privacy::tests::lock_scene_draws_the_pointer_and_delivers_client_cursor_frames",
    ) {
        return;
    }
    use smithay::backend::renderer::damage::OutputDamageTracker;
    use smithay::input::{SeatHandler, pointer::CursorImageStatus};
    use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_manager_v1::ExtSessionLockManagerV1;
    use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

    let mut events = EventLoop::try_new().unwrap();
    let mut state = crate::startup_tests::state(&mut events);
    let output = Output::new(
        "lock-cursor".into(),
        PhysicalProperties {
            serial_number: String::new(),
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "test".into(),
            model: "test".into(),
        },
    );
    state.begin_desktop_transition();
    let changes = state
        .publish_desktop(vec![DesktopOutput {
            output: output.clone(),
            identity: "lock-cursor".into(),
            mode: Mode {
                size: (320, 240).into(),
                refresh: 60_000,
            },
            transform: Transform::Normal,
            scale: Scale::Fractional(1.0),
            position: (0, 0).into(),
        }])
        .unwrap();
    state.finish_desktop_transition(changes);
    let (desktop, mut wire) = window(&mut state, &mut events, 0xffff0000);
    let client = desktop.toplevel().unwrap().wl_surface().client().unwrap();
    let manager = client
        .create_resource::<ExtSessionLockManagerV1, smithay::wayland::GlobalData, Ferese>(
            &state.display_handle,
            1,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    request(&mut wire, manager.id().protocol_id(), 1, &[7], None);
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while !state.session_lock.active() && std::time::Instant::now() < deadline {
        dispatch(&mut events, &mut state);
    }
    assert!(state.session_lock.active());
    state.session_lock.idle_opacity = 0.65;
    state.named_cursors.insert(
        smithay::input::pointer::CursorIcon::Default,
        crate::cursor::fallback_cursor(),
    );
    state
        .seat
        .get_pointer()
        .unwrap()
        .set_location((20.0, 20.0).into());

    let device = EGLDevice::enumerate().unwrap().last().expect("EGL device");
    // SAFETY: The native display comes from a live enumerated EGLDevice and its lifetime is
    // retained by the EGL objects created here.
    let display = unsafe { EGLDisplay::new(device).unwrap() };
    // SAFETY: The fresh EGL context is exclusively owned by this renderer; this test issues GL
    // work only through its renderer methods.
    let mut renderer = unsafe { GlesRenderer::new(EGLContext::new(&display).unwrap()).unwrap() };
    let mut texture =
        Offscreen::<GlesTexture>::create_buffer(&mut renderer, Fourcc::Abgr8888, (320, 240).into())
            .unwrap();

    for scale in [1.0, 1.75] {
        output.change_current_state(None, None, Some(Scale::Fractional(scale)), None);
        for include_cursor in [false, true] {
            let scene = state.sample_frame(&output, Duration::ZERO);
            let elements = crate::render::sampled_output_elements(
                &mut state,
                &mut renderer,
                &output,
                include_cursor,
                &scene,
            );
            let mut target = renderer.bind(&mut texture).unwrap();
            crate::render::redraw_output(&mut renderer, &mut target, &output, &elements).unwrap();
            let mapping = renderer
                .copy_framebuffer(
                    &target,
                    Rectangle::from_size((320, 240).into()),
                    Fourcc::Abgr8888,
                )
                .unwrap();
            let bytes = renderer.map_texture(&mapping).unwrap();
            let white = bytes
                .chunks_exact(4)
                .any(|pixel| pixel == [255, 255, 255, 255]);
            assert_eq!(
                white, include_cursor,
                "cursor above the dim overlay at scale {scale}"
            );
            assert!(
                bytes
                    .chunks_exact(4)
                    .all(|pixel| pixel[0] == pixel[1] && pixel[1] == pixel[2]),
                "desktop content must stay concealed"
            );
        }
    }

    let compositor = client
        .create_resource::<WlCompositor, smithay::wayland::GlobalData, Ferese>(
            &state.display_handle,
            6,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    request(&mut wire, compositor.id().protocol_id(), 0, &[8], None);
    request(&mut wire, 8, 1, &[6, 0, 0], None);
    request(&mut wire, 8, 3, &[9], None);
    request(&mut wire, 8, 6, &[], None);
    dispatch(&mut events, &mut state);
    let cursor = client
        .object_from_protocol_id::<WlSurface>(&state.display_handle, 8)
        .unwrap();
    let seat = state.seat.clone();
    state.cursor_image(&seat, CursorImageStatus::Surface(cursor.clone()));
    let scene = state.sample_frame(&output, Duration::ZERO);
    let elements =
        crate::render::sampled_output_elements(&mut state, &mut renderer, &output, true, &scene);
    let mut target = renderer.bind(&mut texture).unwrap();
    let mut tracker = OutputDamageTracker::from_output(&output);
    let rendered = tracker
        .render_output(
            &mut renderer,
            &mut target,
            0,
            &elements,
            [0.0, 0.0, 0.0, 1.0],
        )
        .unwrap();
    state
        .display_presentation
        .queued(&output, &rendered.states, scene.input_windows(&output));
    state.display_presentation.presented(&output);
    assert_eq!(
        state.callback_outputs().get(&(&cursor).into()),
        Some(&output)
    );
    state.lock_frame_callbacks(&output);
    state.display_handle.flush_clients().unwrap();
    wire.set_nonblocking(true).unwrap();
    let mut replies = Vec::new();
    let _ = wire.read_to_end(&mut replies);
    let mut offset = 0;
    let mut callback_done = false;
    while offset < replies.len() {
        let object = u32::from_ne_bytes(replies[offset..offset + 4].try_into().unwrap());
        let header = u32::from_ne_bytes(replies[offset + 4..offset + 8].try_into().unwrap());
        callback_done |= object == 9 && header & 0xffff == 0;
        offset += (header >> 16) as usize;
    }
    assert!(
        callback_done,
        "the lock owner's cursor must receive its next frame"
    );
}

#[test]
#[ignore = "requires an EGL device; uses a disposable runtime and private protocol clients"]
fn capture_privacy_pixels_and_policy_transitions() {
    if std::env::var_os("FERESE_PRIVACY_TEST_CHILD").is_none() {
        let runtime = tempfile::tempdir().unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "state::capture_privacy::tests::capture_privacy_pixels_and_policy_transitions",
                "--ignored",
                "--nocapture",
            ])
            .env("FERESE_PRIVACY_TEST_CHILD", "1")
            .env("FERESE_ENABLE_SCREENCOPY", "1")
            .env("XDG_RUNTIME_DIR", runtime.path())
            .env("XDG_CONFIG_HOME", runtime.path())
            .env("XDG_STATE_HOME", runtime.path())
            .env_remove("FERESE_SOCKET")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    let mut events = EventLoop::try_new().unwrap();
    let config = crate::config::Config::parse_source(
        "animations { reduced-motion #true; }\nwindow-rule transient=#false floating=#true\n",
    )
    .unwrap()
    .runtime_config()
    .unwrap();
    let mut state = Ferese::new(&mut events, Display::new().unwrap(), config).unwrap();
    let output = Output::new(
        "privacy".into(),
        PhysicalProperties {
            serial_number: String::new(),
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "test".into(),
            model: "test".into(),
        },
    );
    let desktop = DesktopOutput {
        output: output.clone(),
        identity: "privacy".into(),
        mode: Mode {
            size: (320, 240).into(),
            refresh: 60_000,
        },
        transform: Transform::Normal,
        scale: Scale::Fractional(1.0),
        position: (0, 0).into(),
    };
    state.begin_desktop_transition();
    let changes = state.publish_desktop(vec![desktop]).unwrap();
    state.finish_desktop_transition(changes);
    let (secret, mut secret_wire) = window(&mut state, &mut events, 0xffff0000);
    attach_children(&mut state, &mut events, &secret, &mut secret_wire);
    let id = state.windows.ids()[&secret];
    assert!(state.window_content_ready(&secret));
    let device = EGLDevice::enumerate().unwrap().last().expect("EGL device");
    // SAFETY: The native display comes from a live enumerated EGLDevice and its lifetime is
    // retained by the EGL objects created here.
    let display = unsafe { EGLDisplay::new(device).unwrap() };
    let context = EGLContext::new(&display).unwrap();
    // SAFETY: The fresh EGL context is exclusively owned by this renderer; this test issues GL
    // work only through its renderer methods.
    let renderer = Rc::new(RefCell::new(unsafe { GlesRenderer::new(context).unwrap() }));
    state.readbacks.set_test_renderer(renderer.clone());
    let mut display_texture = Offscreen::<GlesTexture>::create_buffer(
        &mut *renderer.borrow_mut(),
        Fourcc::Abgr8888,
        (320, 240).into(),
    )
    .unwrap();
    let mut capture_texture = Offscreen::<GlesTexture>::create_buffer(
        &mut *renderer.borrow_mut(),
        Fourcc::Abgr8888,
        (320, 240).into(),
    )
    .unwrap();
    let has_red = |bytes: &[u8]| bytes.chunks_exact(4).any(|pixel| pixel == [255, 0, 0, 255]);
    assert!(has_red(&pixels(
        &mut state,
        &renderer,
        &mut events,
        &output,
        false,
        &mut display_texture
    )));
    assert!(has_red(&pixels(
        &mut state,
        &renderer,
        &mut events,
        &output,
        true,
        &mut capture_texture
    )));
    let epoch = state.capture_epoch;
    assert!(!state.has_capture_exclusions());
    crate::handlers::window_capture::tests::check_deferred_snapshot(&state, &secret, epoch, true);
    app_id(&mut state, &secret, "ordinary");
    assert_eq!(
        state.capture_epoch, epoch,
        "unmatched metadata must not interrupt capture"
    );
    app_id(&mut state, &secret, "dev.ferese.Authentication");
    assert!(state.capture_protected(&secret));
    assert!(state.has_capture_exclusions());
    assert!(!state.capture_window_allowed(id, epoch));
    crate::handlers::window_capture::tests::check_deferred_snapshot(&state, &secret, epoch, false);
    assert!(
        !has_red(&pixels(
            &mut state,
            &renderer,
            &mut events,
            &output,
            true,
            &mut capture_texture
        )),
        "reused capture target leaked previous pixels"
    );
    assert!(
        has_red(&pixels(
            &mut state,
            &renderer,
            &mut events,
            &output,
            false,
            &mut display_texture
        )),
        "display lost protected content"
    );
    app_id(&mut state, &secret, "ordinary");
    assert!(
        !state.capture_window_allowed(id, epoch),
        "protect then unprotect must invalidate deferred frames"
    );
    assert!(state.capture_window_allowed(id, state.capture_epoch));
    crate::handlers::window_capture::tests::check_deferred_snapshot(&state, &secret, epoch, false);
    assert!(has_red(&pixels(
        &mut state,
        &renderer,
        &mut events,
        &output,
        true,
        &mut capture_texture
    )));
    let rules = crate::config::Config::parse_source(
        "window-rule app-id=\"ordinary\" block-out-from-screencasts=#true\n",
    )
    .unwrap()
    .window_rules()
    .unwrap();
    state.window_rules = rules;
    state.refresh_capture_privacy();
    assert!(state.capture_protected(&secret));
    assert!(!has_red(&pixels(
        &mut state,
        &renderer,
        &mut events,
        &output,
        true,
        &mut capture_texture
    )));
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
        for scale in [1.0, 1.25] {
            output.change_current_state(
                None,
                Some(transform),
                Some(Scale::Fractional(scale)),
                None,
            );
            assert!(
                has_red(&pixels(
                    &mut state,
                    &renderer,
                    &mut events,
                    &output,
                    false,
                    &mut display_texture
                )),
                "display {transform:?}/{scale}"
            );
            assert!(
                !has_red(&pixels(
                    &mut state,
                    &renderer,
                    &mut events,
                    &output,
                    true,
                    &mut capture_texture
                )),
                "capture {transform:?}/{scale}"
            );
        }
    }
    output.change_current_state(
        None,
        Some(Transform::Normal),
        Some(Scale::Fractional(1.0)),
        None,
    );
    // An otherwise unprotected child still inherits its parent's protection.
    let (child, _child_wire) = window(&mut state, &mut events, 0xff00ff00);
    with_states(child.toplevel().unwrap().wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .unwrap()
            .lock()
            .unwrap()
            .parent = Some(secret.toplevel().unwrap().wl_surface().clone());
    });
    state.parent_changed(child.toplevel().unwrap().clone());
    assert!(state.capture_protected(&child));
    let captured = pixels(
        &mut state,
        &renderer,
        &mut events,
        &output,
        true,
        &mut capture_texture,
    );
    assert!(!has_red(&captured));
    assert!(
        !captured
            .chunks_exact(4)
            .any(|pixel| pixel == [0, 255, 0, 255])
    );

    state.set_overview_active(true);
    let overview = pixels(
        &mut state,
        &renderer,
        &mut events,
        &output,
        true,
        &mut capture_texture,
    );
    assert!(!has_red(&overview));
    assert!(
        !overview
            .chunks_exact(4)
            .any(|pixel| pixel == [0, 255, 0, 255])
    );
    state.set_overview_active(false);

    // Region requests go through the same filtered output readback.
    let pending = OutputReadback::queue(
        &mut state,
        &output,
        Rectangle::new((32, 24).into(), (160, 120).into()),
    );
    let scene = state.sample_frame(&output, Duration::ZERO);
    let mut target = std::mem::take(&mut state.nested_capture_texture);
    crate::backends::direct::capture::capture_output(
        &mut state,
        &mut renderer.borrow_mut(),
        &mut target,
        &output,
        &scene,
    );
    state.nested_capture_texture = target;
    let region = pending.wait(&mut events, &mut state);
    assert_eq!((region.width, region.height), (160, 120));
    assert!(
        !region
            .pixels
            .chunks_exact(4)
            .any(|pixel| pixel == [0, 0, 255, 255] || pixel == [0, 255, 0, 255])
    );

    // A transient remains protected when moved to a different monitor.
    let second = Output::new(
        "second".into(),
        PhysicalProperties {
            serial_number: String::new(),
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "test".into(),
            model: "test".into(),
        },
    );
    let mut desktop = state.current_desktop_outputs();
    desktop.push(DesktopOutput {
        output: second.clone(),
        identity: "second".into(),
        mode: Mode {
            size: (320, 240).into(),
            refresh: 60_000,
        },
        transform: Transform::Normal,
        scale: Scale::Fractional(1.0),
        position: (400, 0).into(),
    });
    state.begin_desktop_transition();
    let changes = state.publish_desktop(desktop).unwrap();
    state.finish_desktop_transition(changes);
    let destination = state
        .output_workspaces
        .active_workspace(state.output_ids[&second])
        .unwrap();
    let child_id = state.windows.ids()[&child];
    state
        .workspaces
        .move_window_to_workspace(child_id, destination, ferese_layout::Axis::Horizontal, 0.5)
        .unwrap();
    state.relayout();
    let rect = ferese_layout::Rect::new(440.0, 40.0, 64.0, 48.0);
    state.workspaces.set_floating_rect(child_id, rect).unwrap();
    let mut geometry = ferese_animation::WindowGeometry::new(rect, None);
    geometry.client.committed_size = Some(ferese_animation::ClientSize::from_rect(rect));
    state.windows.set_geometry(child_id, geometry);
    state.space.map_element(child.clone(), (440, 40), false);
    assert!(
        pixels(
            &mut state,
            &renderer,
            &mut events,
            &second,
            false,
            &mut display_texture
        )
        .chunks_exact(4)
        .any(|pixel| pixel == [0, 255, 0, 255])
    );
    assert!(
        !pixels(
            &mut state,
            &renderer,
            &mut events,
            &second,
            true,
            &mut capture_texture
        )
        .chunks_exact(4)
        .any(|pixel| pixel == [0, 255, 0, 255])
    );

    // Unprotected content still captures, including translucent pixels.
    let (public, mut public_wire) = window(&mut state, &mut events, 0x80000080);
    let captured = pixels(
        &mut state,
        &renderer,
        &mut events,
        &output,
        true,
        &mut capture_texture,
    );
    assert!(captured.chunks_exact(4).any(|pixel| pixel[2] > 100));
    assert!(!has_red(&captured));

    // A translucent material over the secret must never reuse display blur.
    use ferese_protocols::material::v1::server::ferese_material_manager_v1::FereseMaterialManagerV1;
    let client = public.toplevel().unwrap().wl_surface().client().unwrap();
    let manager = client
        .create_resource::<FereseMaterialManagerV1, (), Ferese>(&state.display_handle, 1, ())
        .unwrap();
    request(
        &mut public_wire,
        manager.id().protocol_id(),
        1,
        &[7, 2],
        None,
    );
    dispatch(&mut events, &mut state);
    state.theme_settings.material_style = crate::config::MaterialStyle::Translucent;
    state.theme_settings.backdrop_blur = 12.0;
    // Place the glass over the secret, preserving the protocol-owned buffer.
    let public_id = state.windows.ids()[&public];
    let secret_geometry = *state.windows.geometry(&id).unwrap();
    state.windows.set_geometry(public_id, secret_geometry);
    state.space.map_element(
        public.clone(),
        state.space.element_location(&secret).unwrap(),
        true,
    );
    let before = pixels(
        &mut state,
        &renderer,
        &mut events,
        &output,
        false,
        &mut display_texture,
    );
    let protected = pixels(
        &mut state,
        &renderer,
        &mut events,
        &output,
        true,
        &mut capture_texture,
    );
    state.space.unmap_elem(&secret);
    state.space.unmap_elem(&child);
    let absent = pixels(
        &mut state,
        &renderer,
        &mut events,
        &output,
        true,
        &mut capture_texture,
    );
    assert_eq!(
        protected, absent,
        "protected content leaked through a cached material"
    );
    let without = pixels(
        &mut state,
        &renderer,
        &mut events,
        &output,
        false,
        &mut display_texture,
    );
    assert_ne!(
        before, without,
        "fixture must place protected content behind glass"
    );

    // A stationary client's cursor is a separate surface and can outlive its
    // protected toplevel. Never reveal that retained image after an opt-out or close.
    use smithay::input::SeatHandler;
    use smithay::input::pointer::CursorImageStatus;
    use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
    let client = secret.toplevel().unwrap().wl_surface().client().unwrap();
    let compositor = client
        .create_resource::<WlCompositor, smithay::wayland::GlobalData, Ferese>(
            &state.display_handle,
            6,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    request(
        &mut secret_wire,
        compositor.id().protocol_id(),
        0,
        &[13],
        None,
    );
    request(&mut secret_wire, 13, 1, &[6, 0, 0], None);
    request(&mut secret_wire, 13, 6, &[], None);
    dispatch(&mut events, &mut state);
    let cursor = client
        .object_from_protocol_id::<WlSurface>(&state.display_handle, 13)
        .unwrap();
    let seat = state.seat.clone();
    state.cursor_image(&seat, CursorImageStatus::Surface(cursor.clone()));
    assert!(state.capture_cursor_protected());
    app_id(&mut state, &secret, "public-again");
    state.cursor_image(&seat, CursorImageStatus::Surface(cursor.clone()));
    assert!(state.capture_cursor_protected());
    let mut presentation = state.current_window_presentation(id).unwrap();
    presentation.close();
    let snapshot = crate::render::capture_resize_snapshot(
        &mut renderer.borrow_mut(),
        &secret,
        secret.geometry(),
        1.0,
        crate::presentation::SNAPSHOT_BUDGET,
    )
    .unwrap()
    .unwrap();
    state.remove_tiled_window(&secret);
    assert!(state.capture_cursor_protected());
    assert!(
        matches!(&state.cursor_status, CursorImageStatus::Surface(current) if current == &cursor)
    );
    state.cursor_image(&seat, CursorImageStatus::Hidden);
    assert!(!state.capture_cursor_protected());
    assert!(!state.has_capture_exclusions());

    // A close snapshot remains visible after the protected client leaves the
    // registry. The nested readback shortcut must still use the filtered scene.
    state.space.unmap_elem(&public);
    state.space.unmap_elem(&child);
    state.render.closing.push(crate::render::ClosedWindow {
        presentation,
        output: state.output_ids[&output],
        below: None,
        snapshot,
        handoff: None,
        radius: 0.0,
        shape: crate::presentation::CornerShape::Continuous,
        decorations: 0.0,
        fill: None,
        material: None,
    });
    assert!(
        state.has_capture_exclusions(),
        "close snapshot bypassed capture filtering"
    );
    assert!(has_red(&pixels(
        &mut state,
        &renderer,
        &mut events,
        &output,
        false,
        &mut display_texture
    )));
    assert!(!has_red(&pixels(
        &mut state,
        &renderer,
        &mut events,
        &output,
        true,
        &mut capture_texture
    )));
}

#[test]
#[ignore = "requires private Wayland sockets and an offscreen EGL device"]
fn resize_dependencies_protocol_and_pixels() {
    if std::env::var_os("FERESE_RESIZE_TEST_CHILD").is_none() {
        let runtime = tempfile::tempdir().unwrap();
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "state::capture_privacy::tests::resize_dependencies_protocol_and_pixels",
                "--ignored",
                "--nocapture",
            ])
            .env("FERESE_RESIZE_TEST_CHILD", "1")
            .env("FERESE_ENABLE_SCREENCOPY", "1")
            .env("XDG_RUNTIME_DIR", runtime.path())
            .env("XDG_CONFIG_HOME", runtime.path())
            .env("XDG_STATE_HOME", runtime.path())
            .env_remove("FERESE_SOCKET")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    use ferese_core::WorkspaceLayout;
    use ferese_layout::{ColumnWidth, Direction, Rect, ViewportFocusStrategy};
    let mut events = EventLoop::try_new().unwrap();
    let config = crate::config::Config::parse_source("animations { enabled #false; }\nlayout { inner-gap 0; outer-gap 0; }\ntheme { geometry { window-radius 0; border-width 0; focus-ring-width 0; }; }\nwindow-rule app-id=\"resize.secret\" block-out-from-screencasts=#true\n").unwrap().runtime_config().unwrap();
    let mut state = Ferese::new(&mut events, Display::new().unwrap(), config).unwrap();
    state.animation_test_time = Some(Duration::ZERO);
    let output = Output::new(
        "resize".into(),
        PhysicalProperties {
            serial_number: String::new(),
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "test".into(),
            model: "test".into(),
        },
    );
    output.change_current_state(
        Some(Mode {
            size: (320, 240).into(),
            refresh: 60_000,
        }),
        Some(Transform::Normal),
        None,
        None,
    );
    state.space.map_output(&output, (0, 0));
    state.register_output(&output, "resize".into());
    let (a, mut a_wire) = window(&mut state, &mut events, 0xffff0000);
    let (b, mut b_wire) = window(&mut state, &mut events, 0xff00ff00);
    let (c, mut c_wire) = window(&mut state, &mut events, 0xff0000ff);
    let a_id = state.windows.ids()[&a];
    let b_id = state.windows.ids()[&b];
    let c_id = state.windows.ids()[&c];
    let workspace = state.workspaces.workspace_for_window(a_id).unwrap();
    state.focused_window = Some(a_id);
    state.workspaces.focus_window(a_id).unwrap();
    state.relayout();
    state.animations_enabled = true;
    for (_, record) in state.windows.records_mut() {
        record.opening = None;
    }
    let set_width = |state: &mut Ferese, id, width| {
        let ferese_core::WorkspaceLayout::Scrolling(layout) =
            &mut state.workspaces.workspace_mut(workspace).unwrap().layout
        else {
            panic!()
        };
        layout
            .set_column_width(id, ColumnWidth::Fixed(width))
            .unwrap();
        state.relayout();
        state.display_handle.flush_clients().unwrap();
    };
    set_width(&mut state, b_id, 240.0);
    let b_serial = ack_configure(&mut b_wire, 3);
    dispatch(&mut events, &mut state);
    assert!(
        state.windows.transaction(&b_id).is_some(),
        "ack alone must not release"
    );
    // This target uses the first column, independently of the pending later width.
    let bounds = Rect::new(0.0, 0.0, 320.0, 240.0);
    state
        .workspaces
        .center_window(a_id, bounds, state.gap_config, &state.window_constraints())
        .unwrap();
    state.relayout();
    let before = state.viewports[&workspace].motion().current;
    let held_size = state.windows.geometry(&b_id).unwrap().visual.current.width;
    let held_world = state.windows.record(b_id).unwrap().world_x.unwrap().1;
    state.advance_animations_at(Duration::from_millis(16), Duration::ZERO);
    assert_ne!(
        state.viewports[&workspace].motion().current,
        before,
        "independent viewport stalled before delayed buffer commit"
    );
    assert_eq!(
        state.windows.geometry(&b_id).unwrap().visual.current.width,
        held_size
    );
    assert_eq!(
        state.windows.record(b_id).unwrap().world_x.unwrap().1,
        held_world
    );
    // BASELINE_REPRO_END: the stage above also runs against the reviewed base.
    assert!(
        !state
            .presentation_dependencies
            .viewport_blocked(workspace, &state.windows)
    );
    assert!(
        state
            .presentation_dependencies
            .reflow_blocked(b_id, &state.windows)
    );
    assert!(
        state
            .presentation_dependencies
            .reflow_blocked(c_id, &state.windows)
    );
    assert!(
        !state
            .presentation_dependencies
            .reflow_blocked(a_id, &state.windows)
    );
    let geometry = *state.windows.geometry(&b_id).unwrap();
    let position = state.space.element_location(&b).unwrap();
    assert_eq!(position.x, geometry.visual.current.x.round() as i32);
    let point = (
        geometry.visual.current.x + 5.0,
        geometry.visual.current.y + 5.0,
    )
        .into();
    let (hit, origin) = state.window_surface_under(point).unwrap();
    assert_eq!(hit, *b.toplevel().unwrap().wl_surface());
    assert!((origin.x - geometry.visual.current.x).abs() < 0.001);
    let forecast = state.sample_frame(&output, Duration::from_millis(8));
    let predicted = &forecast.windows[&b_id];
    let mut viewport = *state.viewports[&workspace].motion();
    viewport.advance(
        Duration::from_millis(8).mul_f64(state.animation_speed),
        state.viewport_spring_config,
    );
    assert_eq!(
        predicted.geometry.visual.current.x,
        held_world.current - viewport.current
    );
    assert_eq!(predicted.geometry.visual.current.width, held_size);
    assert_eq!(
        predicted.presentation.bounds.current.x,
        predicted.geometry.visual.current.x
    );
    assert_eq!(predicted.presentation.bounds.velocity.x, -viewport.velocity);
    assert_eq!(
        *state.windows.geometry(&b_id).unwrap(),
        geometry,
        "sampling mutated authoritative state"
    );
    let device = EGLDevice::enumerate().unwrap().last().expect("EGL device");
    // SAFETY: The native display comes from a live enumerated EGLDevice and its lifetime is
    // retained by the EGL objects created here.
    let display = unsafe { EGLDisplay::new(device).unwrap() };
    let context = EGLContext::new(&display).unwrap();
    // SAFETY: The fresh EGL context is exclusively owned by this renderer; this test issues GL
    // work only through its renderer methods.
    let renderer = Rc::new(RefCell::new(unsafe { GlesRenderer::new(context).unwrap() }));
    state.readbacks.set_test_renderer(renderer.clone());
    let mut display_texture = Offscreen::<GlesTexture>::create_buffer(
        &mut *renderer.borrow_mut(),
        Fourcc::Abgr8888,
        (320, 240).into(),
    )
    .unwrap();
    let mut capture_texture = Offscreen::<GlesTexture>::create_buffer(
        &mut *renderer.borrow_mut(),
        Fourcc::Abgr8888,
        (320, 240).into(),
    )
    .unwrap();
    // Offscreen fixture explicitly captures the existing native frame; production
    // capture-before-commit is exercised separately in the privacy regression.
    let snapshot = crate::render::capture_resize_snapshot(
        &mut renderer.borrow_mut(),
        &b,
        b.geometry(),
        1.0,
        16 * 1024 * 1024,
    )
    .unwrap();
    state
        .render
        .set_snapshot(b_id, snapshot.expect("native snapshot"));
    app_id(&mut state, &b, "resize.secret");
    state.refresh_capture_privacy();
    state.advance_animations_at(Duration::from_millis(16), Duration::from_millis(16));
    let actual = state.windows.geometry(&b_id).unwrap().visual.current;
    let displayed = pixels(
        &mut state,
        &renderer,
        &mut events,
        &output,
        false,
        &mut display_texture,
    );
    let at = ((actual.y.round() as usize + 5) * 320 + actual.x.round() as usize + 5) * 4;
    assert_eq!(
        &displayed[at..at + 4],
        &[0, 255, 0, 255],
        "live/snapshot pixels did not translate with geometry"
    );
    let captured = pixels(
        &mut state,
        &renderer,
        &mut events,
        &output,
        true,
        &mut capture_texture,
    );
    assert!(
        !captured
            .chunks_exact(4)
            .any(|pixel| pixel == [0, 255, 0, 255]),
        "translated protected snapshot leaked into capture"
    );
    assert!(state.render.snapshot(&b_id).is_some());
    // A second slow client has an independent serial owner.
    set_width(&mut state, c_id, 128.0);
    ack_configure(&mut c_wire, 3);
    dispatch(&mut events, &mut state);
    assert!(state.windows.transaction(&c_id).is_some());
    state.focused_window = Some(c_id);
    state.workspaces.focus_window(c_id).unwrap();
    // Centering a later column now genuinely uses both pending allocated widths.
    state
        .workspaces
        .center_window(c_id, bounds, state.gap_config, &state.window_constraints())
        .unwrap();
    state.relayout();
    assert!(
        state
            .presentation_dependencies
            .viewport_blocked(workspace, &state.windows)
    );
    let frozen = *state.viewports[&workspace].motion();
    state.advance_animations_at(Duration::from_millis(16), Duration::from_millis(32));
    assert_eq!(*state.viewports[&workspace].motion(), frozen);
    // Supersede the acknowledged size, then commit the older acknowledgement.
    set_width(&mut state, b_id, 96.0);
    let latest = *state.windows.transaction(&b_id).unwrap();
    assert!(!latest.accepts(Some(b_serial.into())));
    request(&mut b_wire, 2, 1, &[6, 0, 0], None);
    request(&mut b_wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert_eq!(
        state.windows.transaction(&b_id).unwrap().serial(),
        latest.serial(),
        "obsolete commit released replacement"
    );
    ack_configure(&mut b_wire, 3);
    dispatch(&mut events, &mut state);
    assert!(
        state.windows.transaction(&b_id).is_some(),
        "latest ack without commit released resize"
    );
    state.focused_window = Some(a_id);
    state.workspaces.focus_window(a_id).unwrap();
    // Rapid reversal to the independent first-column target retires viewport waits.
    state
        .workspaces
        .center_window(a_id, bounds, state.gap_config, &state.window_constraints())
        .unwrap();
    state.relayout();
    assert!(
        !state
            .presentation_dependencies
            .viewport_blocked(workspace, &state.windows)
    );
    let frozen = state.viewports[&workspace].motion().current;
    state.advance_animations_at(Duration::from_millis(16), Duration::from_millis(48));
    assert_ne!(state.viewports[&workspace].motion().current, frozen);
    let client = b.toplevel().unwrap().wl_surface().client().unwrap();
    let shm = client
        .create_resource::<WlShm, smithay::wayland::GlobalData, Ferese>(
            &state.display_handle,
            1,
            smithay::wayland::GlobalData,
        )
        .unwrap();
    let mut resized = tempfile::tempfile().unwrap();
    resized
        .write_all(&0xff00ff00u32.to_ne_bytes().repeat(32 * 48))
        .unwrap();
    request(
        &mut b_wire,
        shm.id().protocol_id(),
        0,
        &[7, 32 * 48 * 4],
        Some(resized.as_raw_fd()),
    );
    request(&mut b_wire, 7, 0, &[8, 0, 32, 48, 32 * 4, 0], None);
    request(&mut b_wire, 3, 3, &[0, 0, 32, 48], None);
    request(&mut b_wire, 2, 1, &[8, 0, 0], None);
    request(&mut b_wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert_eq!(b.geometry().size.w, 32, "resized raster was not committed");
    assert!(
        state.windows.transaction(&b_id).is_none(),
        "latest surface commit did not release"
    );
    assert!(
        state.windows.transaction(&c_id).is_some(),
        "one client released another owner"
    );
    // A hidden workspace must not impose its viewport wait on its replacement.
    let other = state.workspaces.create_workspace();
    let output_id = state.output_id(&output).unwrap();
    state
        .output_workspaces
        .assign_workspace(output_id, other)
        .unwrap();
    state.workspaces.activate(other).unwrap();
    state.relayout();
    assert!(
        !state
            .presentation_dependencies
            .viewport_blocked(other, &state.windows)
    );
    state
        .output_workspaces
        .assign_workspace(output_id, workspace)
        .unwrap();
    state.workspaces.activate(workspace).unwrap();
    state.focused_window = Some(a_id);
    state.relayout();
    request(&mut c_wire, 2, 1, &[0, 0, 0], None);
    request(&mut c_wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert!(
        state.windows.transaction(&c_id).is_none(),
        "unmap kept a configure owner"
    );
    // Deliberately unresponsive final configure expires without charging the wait.
    set_width(&mut state, b_id, 112.0);
    ack_configure(&mut b_wire, 3);
    dispatch(&mut events, &mut state);
    assert!(state.windows.transaction(&b_id).is_some());
    state.advance_animations_at(Duration::from_millis(300), Duration::from_secs(1));
    assert!(state.windows.transaction(&b_id).is_none());
    for tick in 1..=160 {
        state.advance_animations_at(
            Duration::from_millis(16),
            Duration::from_secs(1) + Duration::from_millis(tick * 16),
        );
    }
    assert!(
        state.render.snapshot(&b_id).is_none(),
        "resize snapshot outlived settled handoff"
    );
    // Direct panning does not depend on pending widths. Cancelling restores
    // the previous target without appending a width-dependent reversal.
    state.animations_enabled = false;
    set_width(&mut state, a_id, 400.0);
    set_width(&mut state, b_id, 100.0);
    state.focused_window = Some(a_id);
    state.workspaces.focus_window(a_id).unwrap();
    state
        .workspaces
        .center_window(a_id, bounds, state.gap_config, &state.window_constraints())
        .unwrap();
    state.relayout();
    state.animations_enabled = true;
    set_width(&mut state, b_id, 150.0);
    ack_configure(&mut b_wire, 3);
    dispatch(&mut events, &mut state);
    assert!(
        !state
            .presentation_dependencies
            .viewport_blocked(workspace, &state.windows)
    );
    let before_pan = state.presented_window_rect(a_id).unwrap().x;
    state.preview_scroll_gesture(Direction::Right, 0.5);
    assert!(state.scroll_gesture().is_some());
    assert!(
        !state
            .presentation_dependencies
            .viewport_blocked(workspace, &state.windows)
    );
    state.advance_animations_at(Duration::ZERO, state.presentation_now());
    assert!(state.windows.transaction(&b_id).is_some());
    assert!(
        state.presented_window_rect(a_id).unwrap().x < before_pan,
        "direct panning waited for the resized buffer"
    );
    assert!(state.finish_scroll_gesture(true));
    assert!(
        !state
            .presentation_dependencies
            .viewport_blocked(workspace, &state.windows),
        "cancelled preview contaminated restored target"
    );

    // Two shrinking prefixes can jointly cross a page boundary although
    // neither source width does so alone. A normal partial commit must rebuild
    // the remaining graph immediately, without an explicit relayout.
    state.animations_enabled = false;
    state.advance_animations_at(Duration::ZERO, Duration::from_secs(5));
    state.focused_window = Some(b_id);
    state.workspaces.focus_window(b_id).unwrap();
    let (d, mut d_wire) = window(&mut state, &mut events, 0xffffffff);
    let d_id = state.windows.ids()[&d];
    set_width(&mut state, a_id, 160.0);
    set_width(&mut state, b_id, 160.0);
    set_width(&mut state, d_id, 64.0);
    state.focused_window = Some(d_id);
    {
        let WorkspaceLayout::Scrolling(layout) =
            &mut state.workspaces.workspace_mut(workspace).unwrap().layout
        else {
            panic!()
        };
        layout.set_focus_strategy(ViewportFocusStrategy::Paged);
        layout.focus(a_id).unwrap();
        layout.focus(d_id).unwrap();
    }
    state.relayout();
    state.animations_enabled = true;
    set_width(&mut state, a_id, 64.0);
    set_width(&mut state, b_id, 64.0);
    ack_configure(&mut a_wire, 3);
    ack_configure(&mut b_wire, 3);
    dispatch(&mut events, &mut state);
    assert!(
        state
            .presentation_dependencies
            .viewport_blocked(workspace, &state.windows),
        "joint page dependency was lost: layout={:?} a={:?} b={:?}",
        state.workspaces.workspace(workspace).unwrap().layout,
        state.windows.transaction(&a_id),
        state.windows.transaction(&b_id)
    );
    request(&mut a_wire, 2, 1, &[6, 0, 0], None);
    request(&mut a_wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert!(state.windows.transaction(&a_id).is_none());
    assert!(state.windows.transaction(&b_id).is_some());
    assert!(
        !state
            .presentation_dependencies
            .viewport_blocked(workspace, &state.windows),
        "partial commit failed to retire collective page wait"
    );

    // A sibling that commits early still shares the held source width when
    // a stacked column is retargeted. Input ordering must not choose its source.
    state.animations_enabled = false;
    state.advance_animations_at(Duration::ZERO, Duration::from_secs(10));
    {
        let WorkspaceLayout::Scrolling(layout) =
            &mut state.workspaces.workspace_mut(workspace).unwrap().layout
        else {
            panic!()
        };
        layout.move_into_column(d_id, b_id).unwrap();
    }
    set_width(&mut state, b_id, 100.0);
    state.animations_enabled = true;
    set_width(&mut state, b_id, 200.0);
    ack_configure(&mut b_wire, 3);
    ack_configure(&mut d_wire, 3);
    dispatch(&mut events, &mut state);
    request(&mut d_wire, 2, 1, &[6, 0, 0], None);
    request(&mut d_wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert!(state.windows.transaction(&d_id).is_none());
    set_width(&mut state, b_id, 150.0);
    for id in [b_id, d_id] {
        assert_eq!(
            state.windows.transaction(&id).unwrap().column_width(),
            Some((100.0, 150.0)),
            "stacked replacement lost canonical presented source"
        );
    }
    // Fullscreen geometry is output-owned. Keep its own configure barrier,
    // but release it independently of a slow sibling in the same column.
    state.set_window_fullscreen(d_id, true);
    state.display_handle.flush_clients().unwrap();
    ack_configure(&mut d_wire, 3);
    dispatch(&mut events, &mut state);
    assert!(
        state.windows.transaction(&d_id).is_some(),
        "fullscreen ack alone released its barrier"
    );
    let before = state.windows.geometry(&d_id).unwrap().visual.current;
    state.advance_animations_at(Duration::from_millis(16), Duration::from_millis(10016));
    assert_eq!(
        state.windows.geometry(&d_id).unwrap().visual.current,
        before
    );

    request(&mut d_wire, 2, 1, &[6, 0, 0], None);
    request(&mut d_wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert!(
        state.windows.transaction(&d_id).is_none(),
        "own fullscreen configure did not release after commit"
    );
    assert!(
        state.windows.transaction(&b_id).is_some(),
        "other tile must still be pending"
    );
    assert!(
        !state
            .presentation_dependencies
            .reflow_blocked(d_id, &state.windows)
    );
    assert!(state.windows.geometry(&d_id).unwrap().is_zooming());
    // Consume the release tick without charging the preceding client wait.
    state.advance_animations_at(Duration::ZERO, Duration::from_millis(10016));
    state.advance_animations_at(Duration::from_millis(16), Duration::from_millis(10032));
    assert_ne!(
        state.windows.geometry(&d_id).unwrap().visual.current,
        before,
        "fullscreen zoom stayed held by an unrelated column resize after its own commit"
    );

    // Returning to the column needs its real reflow wait again, even after
    // the returning client's own configure has committed.
    state.set_window_fullscreen(d_id, false);
    state.display_handle.flush_clients().unwrap();
    ack_configure(&mut d_wire, 3);
    request(&mut d_wire, 2, 1, &[6, 0, 0], None);
    request(&mut d_wire, 2, 6, &[], None);
    dispatch(&mut events, &mut state);
    assert!(state.windows.transaction(&d_id).is_none());
    assert!(state.windows.transaction(&b_id).is_some());
    assert!(
        state
            .presentation_dependencies
            .reflow_blocked(d_id, &state.windows)
    );
    let returning = state.windows.geometry(&d_id).unwrap().visual.current;
    state.advance_animations_at(Duration::from_millis(16), Duration::from_millis(10048));
    assert_eq!(
        state.windows.geometry(&d_id).unwrap().visual.current,
        returning
    );
}
