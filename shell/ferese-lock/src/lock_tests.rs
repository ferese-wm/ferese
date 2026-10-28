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
