//! Visible clock labels, driven by realtime boundaries and timezone changes.
use std::ffi::{CString, OsString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use cosmic::iced::{Subscription, futures};
use jiff::{Timestamp, Zoned, tz::TimeZone};

const SECOND: i128 = 1_000_000_000;
const MINUTE: i128 = 60 * SECOND;
const UNNAMED_ZONE: &str = "ferese/unnamed-system-zone";

pub fn current_time() -> String {
    labels(Timestamp::now(), &zones().0)
}

pub fn subscription(visible: bool) -> Subscription<String> {
    if visible {
        Subscription::run_with((), stream)
    } else {
        Subscription::none()
    }
}

fn stream(_: &()) -> impl futures::Stream<Item = String> + use<> {
    let (send, receive) = tokio::sync::mpsc::channel(1);
    let stop = match UnixStream::pair() {
        Ok((stop, cancel)) => {
            match std::thread::Builder::new().name("ferese-clock".into()).spawn(move || {
                if let Err(error) = watch(send, cancel) {
                    eprintln!("ferese-shell: clock subscription: {error}");
                }
            }) {
                Ok(_) => Some(stop),
                Err(error) => {
                    eprintln!("ferese-shell: clock worker: {error}");
                    None
                }
            }
        }
        Err(error) => {
            eprintln!("ferese-shell: clock cancellation socket: {error}");
            None
        }
    };
    // Dropping the stream closes the socket and wakes the worker's poll.
    futures::stream::unfold((receive, stop), |(mut receive, stop)| async move {
        Some((receive.recv().await?, (receive, stop)))
    })
}

fn watch(send: tokio::sync::mpsc::Sender<String>, cancel: UnixStream) -> io::Result<()> {
    let timer =
        owned_fd(unsafe { libc::timerfd_create(libc::CLOCK_REALTIME, libc::TFD_CLOEXEC | libc::TFD_NONBLOCK) })?;
    let (mut system, mut paths) = zones();
    let mut timezone = ZoneWatch::new(&paths)?;
    let mut previous = None;
    loop {
        let now = Timestamp::now();
        let labels = labels(now, &system);
        if previous.as_ref() != Some(&labels) {
            previous = Some(labels.clone());
            if send.blocking_send(labels).is_err() {
                return Ok(());
            }
        }
        if arm(&timer, zone_deadline(&now.to_zoned(system.clone())))? {
            continue;
        }
        let mut fds = [
            libc::pollfd {
                fd: timer.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: timezone.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: cancel.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        loop {
            if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, -1) } >= 0 {
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        if fds[2].revents != 0 {
            return Ok(());
        }
        if fds[0].revents != 0 {
            read_timer(&timer)?;
        }
        if fds[1].revents != 0 && timezone.changed()? {
            jiff::tz::db().reset();
            (system, paths) = zones();
            timezone = ZoneWatch::new(&paths)?;
        }
    }
}

fn owned_fd(fd: libc::c_int) -> io::Result<OwnedFd> {
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

fn arm(timer: &OwnedFd, deadline: i128) -> io::Result<bool> {
    let spec = libc::itimerspec {
        it_interval: libc::timespec { tv_sec: 0, tv_nsec: 0 },
        it_value: libc::timespec {
            tv_sec: deadline.div_euclid(SECOND) as _,
            tv_nsec: deadline.rem_euclid(SECOND) as _,
        },
    };
    let flags = libc::TFD_TIMER_ABSTIME | libc::TFD_TIMER_CANCEL_ON_SET;
    if unsafe { libc::timerfd_settime(timer.as_raw_fd(), flags, &spec, std::ptr::null_mut()) } < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ECANCELED) {
            // settime can apply the new deadline yet report an unread clock
            // cancellation. Drain it and resample before waiting again.
            read_timer(timer)?;
            return Ok(true);
        }
        return Err(error);
    }
    Ok(false)
}

fn read_timer(timer: &OwnedFd) -> io::Result<()> {
    let mut expirations = 0u64;
    let result = unsafe { libc::read(timer.as_raw_fd(), (&mut expirations as *mut u64).cast(), 8) };
    if result < 0 {
        let error = io::Error::last_os_error();
        // A wall-clock adjustment cancels the timer. Recompute labels/deadline
        // just as after expiration; CLOCK_REALTIME also advances during suspend.
        if !matches!(error.raw_os_error(), Some(libc::ECANCELED | libc::EAGAIN | libc::EINTR)) {
            return Err(error);
        }
    }
    Ok(())
}

fn zones() -> (TimeZone, Vec<PathBuf>) {
    let mut paths = vec![PathBuf::from("/etc/localtime"), PathBuf::from("/etc/timezone")];
    // Load the file directly: Jiff's system timezone cache lasts five minutes.
    let system = match std::env::var("TZ") {
        Ok(zone) if zone.is_empty() => TimeZone::UTC,
        Ok(zone) => named_zone(zone.trim_start_matches(':'), &mut paths).unwrap_or_else(TimeZone::system),
        Err(_) => file_zone(Path::new("/etc/localtime"), &mut paths).unwrap_or_else(TimeZone::system),
    };
    (system, paths)
}

fn named_zone(name: &str, paths: &mut Vec<PathBuf>) -> Option<TimeZone> {
    // Jiff treats UTC case-insensitively and gives it the Etc/UTC identity.
    if name.eq_ignore_ascii_case("UTC") {
        return Some(TimeZone::UTC);
    }
    for root in ["/usr/share/zoneinfo", "/usr/share/lib/zoneinfo", "/etc/zoneinfo"] {
        let path = Path::new(root).join(name);
        if let Some(zone) = file_zone(&path, paths) {
            return Some(zone);
        }
    }
    file_zone(Path::new(name), paths)
        .or_else(|| TimeZone::posix(name).ok())
        .or_else(|| TimeZone::get(name).ok())
}

fn zone_identifier(path: &Path) -> Option<&str> {
    path.to_str()
        .and_then(|path| path.split_once("zoneinfo/").map(|(_, name)| name))
}

fn file_zone(path: &Path, paths: &mut Vec<PathBuf>) -> Option<TimeZone> {
    let data = std::fs::read(path).ok()?;
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let name = zone_identifier(path)
        .or_else(|| zone_identifier(&resolved))
        .unwrap_or(UNNAMED_ZONE);
    let zone = TimeZone::tzif(name, &data).ok()?;
    paths.push(path.to_owned());
    paths.push(resolved);
    Some(zone)
}

fn labels(now: Timestamp, system: &TimeZone) -> String {
    ferese_theme::calendar::format_bar_time(&now.to_zoned(system.clone()))
}

fn zone_deadline(now: &Zoned) -> i128 {
    let next = now.timestamp().as_nanosecond() + MINUTE
        - i128::from(now.second()) * SECOND
        - i128::from(now.subsec_nanosecond());
    now.time_zone()
        .following(now.timestamp())
        .next()
        .map_or(next, |transition| next.min(transition.timestamp().as_nanosecond()))
}

struct ZoneWatch {
    fd: OwnedFd,
    names: Vec<(libc::c_int, OsString)>,
}

impl ZoneWatch {
    fn new(paths: &[PathBuf]) -> io::Result<Self> {
        let fd = owned_fd(unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) })?;
        let mut names = Vec::new();
        for path in paths {
            let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
                continue;
            };
            let Ok(parent) = CString::new(parent.as_os_str().as_bytes()) else {
                continue;
            };
            let mask = libc::IN_CLOSE_WRITE
                | libc::IN_MOVED_TO
                | libc::IN_CREATE
                | libc::IN_DELETE
                | libc::IN_ATTRIB
                | libc::IN_MOVE_SELF
                | libc::IN_DELETE_SELF;
            let wd = unsafe { libc::inotify_add_watch(fd.as_raw_fd(), parent.as_ptr(), mask) };
            if wd >= 0 {
                names.push((wd, name.to_owned()));
            }
        }
        Ok(Self { fd, names })
    }

    fn changed(&self) -> io::Result<bool> {
        let mut buffer = [0u8; 8192];
        let mut changed = false;
        loop {
            let size = unsafe { libc::read(self.fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) };
            if size < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::WouldBlock {
                    return Ok(changed);
                }
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if size == 0 {
                return Ok(changed);
            }
            let mut offset = 0;
            while offset + std::mem::size_of::<libc::inotify_event>() <= size as usize {
                let event = unsafe { (buffer.as_ptr().add(offset) as *const libc::inotify_event).read_unaligned() };
                offset += std::mem::size_of::<libc::inotify_event>();
                let end = offset + event.len as usize;
                if end > size as usize {
                    break;
                }
                let name = &buffer[offset..end];
                let name = &name[..name.iter().position(|byte| *byte == 0).unwrap_or(name.len())];
                changed |= event.mask
                    & (libc::IN_Q_OVERFLOW | libc::IN_IGNORED | libc::IN_MOVE_SELF | libc::IN_DELETE_SELF)
                    != 0
                    || self
                        .names
                        .iter()
                        .any(|(wd, watched)| *wd == event.wd && watched.as_bytes() == name);
                offset = end;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn panel_subscription_stops_when_all_panels_are_hidden() {
        assert_eq!(subscription(false).units(), 0);
        assert_eq!(subscription(true).units(), 1);
    }

    #[test]
    fn localtime_replacement_loads_new_zone_without_system_cache() {
        let mut paths = Vec::new();
        named_zone("America/New_York", &mut paths).unwrap();
        let eastern = std::fs::read(paths.first().unwrap()).unwrap();
        paths.clear();
        named_zone("Asia/Kathmandu", &mut paths).unwrap();
        let nepal = std::fs::read(paths.first().unwrap()).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("localtime");
        std::fs::write(&path, eastern).unwrap();
        let watcher = ZoneWatch::new(std::slice::from_ref(&path)).unwrap();
        let now: Timestamp = "2026-10-04T12:34:56Z".parse().unwrap();
        let first = file_zone(&path, &mut Vec::new()).unwrap();
        let replacement = dir.path().join("replacement");
        std::fs::write(&replacement, nepal).unwrap();
        std::fs::rename(replacement, &path).unwrap();
        assert!(watcher.changed().unwrap());
        let second = file_zone(&path, &mut Vec::new()).unwrap();
        assert_eq!(labels(now, &first), "4 oct, 8:34 am");
        assert_eq!(labels(now, &second), "4 oct, 6:19 pm");
    }

    #[test]
    fn realtime_jumps_recompute_boundaries_and_timezone_labels() {
        let initial: Timestamp = "2026-10-04T12:34:56Z".parse().unwrap();
        let first = labels(initial, &TimeZone::UTC);
        for (stamp, expected) in [
            ("2026-10-04T11:20:01Z", "2026-10-04T11:21:00Z"),
            ("2026-10-04T15:40:01Z", "2026-10-04T15:41:00Z"),
        ] {
            let now: Timestamp = stamp.parse().unwrap();
            assert_ne!(labels(now, &TimeZone::UTC), first);
            assert_eq!(
                zone_deadline(&now.to_zoned(TimeZone::UTC)),
                expected.parse::<Timestamp>().unwrap().as_nanosecond()
            );
        }
        let zone = TimeZone::get("Asia/Kathmandu").unwrap();
        assert_ne!(labels(initial, &zone), first);
        assert_eq!(labels(initial, &zone), "4 oct, 6:19 pm");
        assert_eq!(
            labels(initial, &TimeZone::UTC),
            labels(initial + jiff::SignedDuration::from_secs(1), &TimeZone::UTC)
        );
    }

    #[test]
    fn boundaries_handle_midnight_dst_and_offsets_with_seconds() {
        for (stamp, expected) in [
            ("2026-10-04T23:59:59.9Z", "2026-10-05T00:00:00Z"),
            ("2026-10-04T12:34:00Z", "2026-10-04T12:35:00Z"),
        ] {
            let now: Timestamp = stamp.parse().unwrap();
            assert_eq!(
                zone_deadline(&now.to_zoned(TimeZone::UTC)),
                expected.parse::<Timestamp>().unwrap().as_nanosecond()
            );
        }
        let zone = TimeZone::get("America/New_York").unwrap();
        for (stamp, expected, label) in [
            ("2026-03-08T06:59:59Z", "2026-03-08T07:00:00Z", "8 mar, 3:00 am"),
            ("2026-11-01T05:59:59Z", "2026-11-01T06:00:00Z", "1 nov, 1:00 am"),
        ] {
            let now: Timestamp = stamp.parse().unwrap();
            let expected: Timestamp = expected.parse().unwrap();
            assert_eq!(zone_deadline(&now.to_zoned(zone.clone())), expected.as_nanosecond());
            assert_eq!(labels(expected, &zone), label);
        }
        let zone = TimeZone::fixed(jiff::tz::Offset::from_seconds(30).unwrap());
        let now: Timestamp = "2026-10-04T12:34:20Z".parse().unwrap();
        assert_eq!(
            zone_deadline(&now.to_zoned(zone)),
            "2026-10-04T12:34:30Z".parse::<Timestamp>().unwrap().as_nanosecond()
        );
    }

    #[test]
    fn timezone_watch_filters_unrelated_files_and_survives_atomic_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("localtime");
        std::fs::write(&path, b"old").unwrap();
        let watcher = ZoneWatch::new(std::slice::from_ref(&path)).unwrap();
        std::fs::write(dir.path().join("unrelated"), b"data").unwrap();
        assert!(!watcher.changed().unwrap());
        let replacement = dir.path().join("replacement");
        std::fs::write(&replacement, b"new").unwrap();
        std::fs::rename(replacement, &path).unwrap();
        assert!(watcher.changed().unwrap());
        assert!(!watcher.changed().unwrap());
        std::fs::write(&path, b"updated").unwrap();
        assert!(watcher.changed().unwrap());
    }

    #[test]
    fn absolute_timer_and_subscription_drop_wake_without_recurring_poll() {
        let timer =
            owned_fd(unsafe { libc::timerfd_create(libc::CLOCK_REALTIME, libc::TFD_CLOEXEC | libc::TFD_NONBLOCK) })
                .unwrap();
        arm(&timer, Timestamp::now().as_nanosecond() - SECOND).unwrap();
        let mut poll = libc::pollfd {
            fd: timer.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut poll, 1, 1000) }, 1);
        read_timer(&timer).unwrap();
        // Reading an already drained timer is harmless (EAGAIN).
        read_timer(&timer).unwrap();

        let (stop, cancel) = UnixStream::pair().unwrap();
        let (send, mut receive) = tokio::sync::mpsc::channel(1);
        let (done_send, done_receive) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            done_send.send(watch(send, cancel)).unwrap();
        });
        assert!(receive.blocking_recv().is_some());
        drop(stop);
        done_receive.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
        assert!(receive.blocking_recv().is_none());
    }
}
