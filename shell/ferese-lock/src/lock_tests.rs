use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use cosmic::Application;
use cosmic::app::{Core, Task};
use cosmic::iced::Event;
use cosmic::iced::event::{PlatformSpecific, wayland};
use cosmic::iced::futures::{StreamExt, executor};
use cosmic::iced::runtime::platform_specific;
use cosmic::iced::runtime::{self as iced_runtime, Action};
use zeroize::Zeroizing;

use crate::password::Password;
use crate::{AuthState, Flags, Locker, Message, runtime};

fn locker(config: Option<PathBuf>) -> (Locker, Task<Message>) {
    Locker::init(
        Core::default(),
        Flags {
            password: Password::try_new().expect("test password pages can be locked"),
            preview: false,
            user: "test-locker".into(),
            config,
            completed: Arc::new(AtomicBool::new(false)),
        },
    )
}

fn event(event: wayland::SessionLockEvent) -> Event {
    Event::PlatformSpecific(PlatformSpecific::Wayland(wayland::Event::SessionLock(
        event,
    )))
}

#[test]
fn first_action_requests_lock_without_reading_configuration_or_artwork() {
    let directory = tempfile::tempdir().expect("private configuration directory");
    let path = directory.path().join("blocked-config");
    assert!(
        Command::new("mkfifo")
            .arg(&path)
            .status()
            .expect("create FIFO")
            .success()
    );
    let (mut locker, task) = locker(Some(path));
    let mut stream = iced_runtime::task::into_stream(task).expect("initial lock request");
    let action = executor::block_on(stream.next()).expect("lock action");
    assert!(matches!(
        action,
        Action::PlatformSpecific(platform_specific::Action::Wayland(
            platform_specific::wayland::Action::SessionLock(
                platform_specific::wayland::session_lock::Action::Lock
            )
        ))
    ));
    assert!(
        executor::block_on(stream.next()).is_none(),
        "startup must not queue artwork before protection"
    );
    assert!(!locker.appearance_loaded);
    let task = locker.handle_event(event(wayland::SessionLockEvent::Locked));
    assert!(locker.confirmation.confirmed());
    assert_eq!(task.units(), 1, "artwork starts only after confirmation");
}

#[test]
fn refusal_and_unsupported_protocol_exit_as_failures() {
    for refused in [
        wayland::SessionLockEvent::Finished,
        wayland::SessionLockEvent::NotSupported,
    ] {
        let (mut locker, _) = locker(None);
        let task = locker.handle_event(event(refused));
        let mut stream = iced_runtime::task::into_stream(task).expect("refusal exits the runner");
        assert!(matches!(
            executor::block_on(stream.next()),
            Some(Action::Exit)
        ));
        assert!(runtime::check_completion(locker.completed.load(Ordering::Acquire)).is_err());
    }
    let (mut locker, _) = locker(None);
    locker.state = AuthState::Unlocking;
    let _ = locker.handle_event(event(wayland::SessionLockEvent::Unlocked));
    assert!(runtime::check_completion(locker.completed.load(Ordering::Acquire)).is_ok());
}

#[test]
fn old_idle_deadlines_cannot_clear_new_input_and_current_deadline_wipes_it() {
    let (mut locker, _) = locker(None);
    let _ = locker.update(Message::Input(Zeroizing::new("first".into())));
    let earlier = Instant::now() - Duration::from_secs(11);
    locker.input_at = Some(earlier);
    let _ = locker.update(Message::Input(Zeroizing::new("second".into())));
    let current = locker.input_at.expect("typing arms the idle deadline");
    let _ = locker.update(Message::InputExpired(earlier));
    assert_eq!(locker.password.as_bytes_with_nul(), b"firstsecond\0");
    let _ = locker.update(Message::InputExpired(current));
    assert!(locker.password.is_empty());
    assert!(locker.input_at.is_none());
    let _ = locker.update(Message::Input(Zeroizing::new("again".into())));
    let _ = locker.update(Message::ClearPassword);
    assert!(locker.password.is_empty());
}

#[test]
fn cooldown_accepts_edits_but_rejects_submission_and_late_success() {
    let (mut locker, _) = locker(None);
    locker.state = AuthState::Checking;
    let _ = locker.update(Message::Authenticated(crate::auth::Outcome::Rejected));
    let _ = locker.update(Message::Input(Zeroizing::new("corrected".into())));
    assert_eq!(locker.password.as_bytes_with_nul(), b"corrected\0");
    assert!(!locker.state.may_submit());
    assert_eq!(locker.status, "Password not accepted.");
    let _ = locker.update(Message::Backspace);
    assert_eq!(locker.password.as_bytes_with_nul(), b"correcte\0");
    assert_eq!(locker.update(Message::Submit).units(), 0);
    let _ = locker.update(Message::Authenticated(crate::auth::Outcome::Accepted));
    assert!(matches!(locker.state, AuthState::Rejected(_)));
    assert_eq!(locker.status, "Password not accepted.");
}

#[test]
fn submit_enablement_and_layout_refresh_follow_actual_state() {
    let (mut locker, _) = locker(None);
    assert!(!locker.can_submit());
    let _ = locker.update(Message::Input(Zeroizing::new("test-only".into())));
    assert!(!locker.can_submit(), "unconfirmed lock cannot submit");
    locker
        .confirmation
        .observe(&wayland::SessionLockEvent::Locked);
    assert!(locker.can_submit());
    locker.state = AuthState::Checking;
    assert!(!locker.can_submit());
    locker.state = AuthState::Rejected(Instant::now());
    assert!(!locker.can_submit());
    assert_eq!(locker.refresh_keyboard_layout().units(), 1);
    assert_eq!(locker.refresh_keyboard_layout().units(), 0);
    assert_eq!(
        locker
            .update(Message::KeyboardLayout(Some("English (US)".into())))
            .units(),
        1
    );
    assert_eq!(
        locker
            .update(Message::KeyboardLayout(Some("French".into())))
            .units(),
        0
    );
    assert_eq!(locker.keyboard_layout.as_deref(), Some("French"));
    assert!(!locker.layout_pending);
}

#[test]
fn lock_controls_remain_visible_and_accessible_over_white_wallpaper() {
    use cosmic::iced::advanced::renderer::{Headless, Renderer as _};
    use cosmic::iced::advanced::widget::Tree;
    use cosmic::iced::advanced::{Layout, layout, mouse, renderer};
    use cosmic::iced::{Color, Font, Pixels, Rectangle, Size};
    use iced_accessibility::accesskit::Role;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("render runtime");
    let mut renderer = runtime
        .block_on(<cosmic::Renderer as Headless>::new(
            Font::default(),
            Pixels(16.),
            None,
        ))
        .expect("headless renderer");
    let (mut locker, _) = locker(None);
    locker.appearance.dim = 0.;
    locker.appearance.wallpaper = cosmic::widget::image::Handle::from_rgba(1, 1, vec![255; 4]);
    locker.appearance.high_contrast = true;
    locker.appearance.reduce_transparency = true;
    locker.caps_lock = true;
    locker.keyboard_layout = Some("English (US)".into());
    locker.state = AuthState::Rejected(Instant::now());
    locker.status = "Password not accepted.";
    assert!(locker.password.push_str("test-only"));
    for (width, height) in [(320., 300.), (640., 360.), (1280., 720.)] {
        for scale in [1., 1.25, 1.5] {
            let bounds = Rectangle::with_size(Size::new(width, height));
            let mut view = locker.screen_content(bounds.size());
            let mut tree = Tree::new(&view);
            let layout = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, bounds.size()),
            );
            let accessible = view.as_widget().a11y_nodes(
                Layout::new(&layout),
                &tree,
                mouse::Cursor::Unavailable,
            );
            let nodes = accessible
                .root()
                .iter()
                .chain(accessible.children())
                .map(|n| n.node())
                .collect::<Vec<_>>();
            for label in [
                "Password",
                "Unlock",
                "Caps Lock is on",
                "English (US)",
                "Password not accepted. You can edit now and submit again shortly.",
            ] {
                let node = nodes
                    .iter()
                    .find(|n| n.label() == Some(label))
                    .unwrap_or_else(|| panic!("missing accessible label: {label}"));
                let rect = node.bounds().expect("control bounds");
                assert!(
                    rect.x0 >= 0.
                        && rect.y0 >= 0.
                        && rect.x1 <= width as f64
                        && rect.y1 <= height as f64,
                    "clipped {label} at {width}x{height}: {rect:?}"
                );
                if label == "Unlock" {
                    assert!(rect.width() >= 24. && rect.height() >= 24.);
                    assert!(node.is_disabled());
                }
                if label == "Password" {
                    assert_eq!(node.role(), Role::PasswordInput);
                    assert!(node.value().is_none());
                }
            }
            renderer.reset(bounds);
            view.as_widget().draw(
                &tree,
                &mut renderer,
                &locker.appearance.theme(),
                &renderer::Style {
                    text_color: Color::WHITE,
                    icon_color: Color::WHITE,
                    scale_factor: scale as f64,
                },
                Layout::new(&layout),
                mouse::Cursor::Unavailable,
                &bounds,
            );
            let physical = Size::new((width * scale) as u32, (height * scale) as u32);
            let pixels = Headless::screenshot(&mut renderer, physical, scale, Color::WHITE);
            if let Some(directory) = std::env::var_os("FERESE_LOCK_CAPTURE_DIR") {
                image::save_buffer(
                    std::path::Path::new(&directory)
                        .join(format!("lock-{width}-{height}-{scale}.png")),
                    &pixels,
                    physical.width,
                    physical.height,
                    image::ColorType::Rgba8,
                )
                .expect("render capture");
            }
            for label in [
                "Caps Lock is on",
                "English (US)",
                "Password not accepted. You can edit now and submit again shortly.",
            ] {
                let rect = nodes
                    .iter()
                    .find(|n| n.label() == Some(label))
                    .expect("status node")
                    .bounds()
                    .expect("bounds");
                let mut light = 0;
                let mut dark = 0;
                for y in
                    (rect.y0 * scale as f64).ceil() as u32..(rect.y1 * scale as f64).floor() as u32
                {
                    for x in (rect.x0 * scale as f64).ceil() as u32
                        ..(rect.x1 * scale as f64).floor() as u32
                    {
                        let rgb = &pixels[((y * physical.width + x) * 4) as usize..][..3];
                        light += usize::from(rgb.iter().all(|v| *v > 200));
                        dark += usize::from(rgb.iter().all(|v| *v < 30));
                    }
                }
                assert!(
                    light > 10 && dark > 10,
                    "missing readable text for {label} at {width}x{height} scale {scale}: {light}/{dark}"
                );
            }
        }
    }
}

#[test]
fn long_password_masks_stay_on_one_line_inside_the_field() {
    use cosmic::iced::advanced::renderer::{Headless, Renderer as _};
    use cosmic::iced::advanced::widget::Tree;
    use cosmic::iced::advanced::{Layout, layout, mouse, renderer};
    use cosmic::iced::{Color, Font, Pixels, Rectangle, Size};
    use iced_accessibility::accesskit::Role;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("render runtime");
    let mut renderer = runtime
        .block_on(<cosmic::Renderer as Headless>::new(
            Font::default(),
            Pixels(14.),
            None,
        ))
        .expect("headless renderer");
    let (mut locker, _) = locker(None);
    for size in [Size::new(320., 300.), Size::new(1280., 720.)] {
        for scale in [1., 1.25, 1.5] {
            let bounds = Rectangle::with_size(size);
            let physical = Size::new((size.width * scale) as u32, (size.height * scale) as u32);
            let mut captures = Vec::new();
            for length in [1, 4095] {
                locker.password.clear();
                assert!(locker.password.push_str(&"x".repeat(length)));
                let mut view = locker.screen_content(size);
                let mut tree = Tree::new(&view);
                let node = view.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, size),
                );
                let accessible = view.as_widget().a11y_nodes(
                    Layout::new(&node),
                    &tree,
                    mouse::Cursor::Unavailable,
                );
                let entry = accessible
                    .root()
                    .iter()
                    .chain(accessible.children())
                    .map(|node| node.node())
                    .find(|node| node.role() == Role::PasswordInput)
                    .expect("password entry remains accessible");
                assert!(entry.value().is_none());
                assert!(entry.children().is_empty());
                let field = entry.bounds().expect("field bounds");
                renderer.reset(bounds);
                view.as_widget().draw(
                    &tree,
                    &mut renderer,
                    &locker.appearance.theme(),
                    &renderer::Style {
                        text_color: Color::WHITE,
                        icon_color: Color::WHITE,
                        scale_factor: scale as f64,
                    },
                    Layout::new(&node),
                    mouse::Cursor::Unavailable,
                    &bounds,
                );
                captures.push((
                    field,
                    Headless::screenshot(&mut renderer, physical, scale, Color::BLACK),
                ));
            }
            let (field, short) = &captures[0];
            let (long_field, long) = &captures[1];
            assert_eq!(
                field, long_field,
                "password length must not move the controls"
            );
            let mut changed_rows = Vec::new();
            for y in 0..physical.height {
                let mut row_changed = false;
                for x in 0..physical.width {
                    let offset = ((y * physical.width + x) * 4) as usize;
                    if short[offset..offset + 4] == long[offset..offset + 4] {
                        continue;
                    }
                    assert!(
                        x >= (field.x0 * scale as f64).floor() as u32
                            && x < (field.x1 * scale as f64).ceil() as u32
                            && y >= (field.y0 * scale as f64).floor() as u32
                            && y < (field.y1 * scale as f64).ceil() as u32,
                        "password mask escaped its field at ({x}, {y}), size {size:?}, scale {scale}",
                    );
                    row_changed = true;
                }
                if row_changed {
                    changed_rows.push(y);
                }
            }
            let first = changed_rows
                .first()
                .expect("additional bullets must be visible");
            let last = changed_rows
                .last()
                .expect("additional bullets must be visible");
            assert!(
                last - first < (14. * scale).ceil() as u32,
                "mask must occupy only one line"
            );
        }
    }
}
