use std::cell::RefCell;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use smithay::reexports::input::LibinputInterface;

use super::*;

#[derive(Clone, Default)]
struct DeniedAccess(Rc<RefCell<Vec<PathBuf>>>);

impl LibinputInterface for DeniedAccess {
    fn open_restricted(&mut self, path: &Path, _: i32) -> Result<OwnedFd, i32> {
        self.0.borrow_mut().push(path.to_owned());
        // Observe enumeration without opening or intercepting real hardware.
        Err(libc::EACCES)
    }

    fn close_restricted(&mut self, _: OwnedFd) {
        panic!("the test never opens a device");
    }
}

#[test]
#[ignore = "requires udev input devices; records enumeration without opening hardware"]
fn wake_reenumerates_devices_when_the_seat_never_paused() {
    let access = DeniedAccess::default();
    let failures = OpenFailures::default();
    let mut context = Libinput::new_with_udev(Access {
        inner: access.clone(),
        failures: failures.clone(),
    });
    context.udev_assign_seat("seat0").unwrap();
    let initial = std::mem::take(&mut *access.0.borrow_mut());
    assert!(
        !initial.is_empty(),
        "this probe needs enumerated input devices"
    );

    // This is the old behavior: an active context does not discover hardware
    // again, including devices that failed to reopen during a system wake.
    context.resume().unwrap();
    assert!(access.0.borrow().is_empty());

    for _ in 0..3 {
        assert_eq!(
            refresh_context(&mut context, &failures, &mut |_| {
                panic!("denied devices must not produce input events");
            }),
            Err(Failure::Devices),
            "successful udev enumeration must not hide individual opening failures"
        );
        let mut reopened = std::mem::take(&mut *access.0.borrow_mut());
        let mut expected = initial.clone();
        reopened.sort();
        expected.sort();
        assert_eq!(reopened, expected);
    }
}

#[test]
fn failed_resume_can_recover_without_another_seat_activation() {
    if !crate::startup_tests::private_runtime(
        "backends::direct::input_resume::tests::failed_resume_can_recover_without_another_seat_activation",
    ) {
        return;
    }
    let failures = OpenFailures::default();
    let mut context = Libinput::new_with_udev(Access {
        inner: DeniedAccess::default(),
        failures: failures.clone(),
    });
    context.udev_assign_seat("ferese-test-empty-seat").unwrap();
    context.suspend();

    let mut limit = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    // Safety: getrlimit initializes the provided writable rlimit on success.
    let result = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, limit.as_mut_ptr()) };
    assert_eq!(result, 0);
    // Safety: the successful getrlimit call initialized this value.
    let limit = unsafe { limit.assume_init() };
    let exhausted = libc::rlimit {
        rlim_cur: 0,
        ..limit
    };
    // Safety: this child process changes only its own soft fd limit, then
    // restores it before assertions or further runtime work.
    let result = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &exhausted) };
    assert_eq!(result, 0);
    let result = resume_context(&mut context, &failures, &mut |_| panic!("empty seat"));
    // Safety: restoring the previous limit uses the initialized rlimit above.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
    assert_eq!(result, Err(Failure::Monitor));
    refresh_context(&mut context, &failures, &mut |_| panic!("empty seat")).unwrap();
}

#[test]
fn failed_opens_retain_errno_until_reopened_or_unplugged() {
    struct ScriptedAccess {
        error: Option<i32>,
        closed: usize,
    }
    impl LibinputInterface for ScriptedAccess {
        fn open_restricted(&mut self, _: &Path, _: i32) -> Result<OwnedFd, i32> {
            if let Some(error) = self.error {
                Err(error)
            } else {
                Ok(std::fs::File::open("/dev/null").unwrap().into())
            }
        }
        fn close_restricted(&mut self, fd: OwnedFd) {
            self.closed += 1;
            drop(fd);
        }
    }
    let first = tempfile::NamedTempFile::new().unwrap();
    let second = tempfile::NamedTempFile::new().unwrap();
    let failures = OpenFailures::default();
    let mut access = Access {
        inner: ScriptedAccess {
            error: Some(libc::EACCES),
            closed: 0,
        },
        failures: failures.clone(),
    };
    assert_eq!(
        access.open_restricted(first.path(), 0).unwrap_err(),
        libc::EACCES
    );
    assert!(failures.has_present_devices());
    assert_eq!(failures.0.borrow().get(first.path()), Some(&libc::EACCES));
    access.inner.error = Some(libc::ENODEV);
    assert_eq!(
        access.open_restricted(second.path(), 0).unwrap_err(),
        libc::ENODEV
    );
    access.inner.error = None;
    let fd = access.open_restricted(first.path(), 0).unwrap();
    access.close_restricted(fd);
    assert_eq!(access.inner.closed, 1);
    assert!(!failures.0.borrow().contains_key(first.path()));
    assert!(
        failures.has_present_devices(),
        "another failed device still exists"
    );
    drop(second);
    assert!(
        !failures.has_present_devices(),
        "unplugged devices need no retry"
    );
}

#[test]
fn listener_recovery_distinguishes_sleep_from_service_restart_and_clock_sampling_delay() {
    let previous = SleepOffset {
        lower: 1_000,
        upper: 1_001,
    };
    assert!(!previous.advanced_to(SleepOffset {
        lower: 1_001,
        upper: 1_002
    }));
    assert!(!previous.advanced_to(SleepOffset {
        lower: 900,
        upper: 1_200
    }));
    assert!(previous.advanced_to(SleepOffset {
        lower: 61_000,
        upper: 61_002
    }));
    let recovered = SleepOffset {
        lower: 61_000,
        upper: 61_002,
    };
    assert!(!recovered.advanced_to(SleepOffset {
        lower: 61_001,
        upper: 61_003
    }));
    assert!(recovered.advanced_to(SleepOffset {
        lower: 91_000,
        upper: 91_001
    }));
    assert!(
        SleepOffset::sample().is_some(),
        "Linux boot and monotonic clocks are available"
    );
}

#[test]
fn device_retries_have_bounded_backoff_and_a_busy_wait_deadline() {
    let start = Instant::now();
    let mut retries = Retries::new(Failure::Devices, start);
    assert!(retries.can_wait(start + Duration::from_secs(9)));
    assert!(!retries.can_wait(start + RETRY_LIFETIME));
    assert_eq!(
        retries.failed(Failure::Devices, start + Duration::from_secs(1)),
        Some(Duration::from_secs(2))
    );
    assert_eq!(
        retries.failed(Failure::Devices, start + Duration::from_secs(3)),
        Some(Duration::from_secs(4))
    );
    assert_eq!(
        retries.failed(Failure::Devices, start + Duration::from_secs(7)),
        None
    );
}

#[test]
fn monitor_retries_continue_and_give_remaining_devices_their_own_budget() {
    let start = Instant::now();
    let mut retries = Retries::new(Failure::Monitor, start);
    for seconds in [1, 5, 9, 13, 17, 21, 25] {
        let now = start + Duration::from_secs(seconds);
        assert!(retries.can_wait(now));
        assert_eq!(
            retries.failed(Failure::Monitor, now),
            Some(Duration::from_secs(4))
        );
    }
    let restored = start + Duration::from_secs(30);
    assert_eq!(
        retries.failed(Failure::Devices, restored),
        Some(Duration::from_secs(2))
    );
    assert!(retries.can_wait(restored + Duration::from_secs(9)));
    assert!(!retries.can_wait(restored + RETRY_LIFETIME));
    assert_eq!(
        retries.failed(Failure::Devices, restored),
        Some(Duration::from_secs(4))
    );
    assert_eq!(retries.failed(Failure::Devices, restored), None);
    assert_eq!(
        retries.failed(Failure::Monitor, restored),
        Some(Duration::from_secs(4))
    );
    assert_eq!(
        retries.failed(Failure::Devices, restored),
        Some(Duration::from_secs(2))
    );
}
