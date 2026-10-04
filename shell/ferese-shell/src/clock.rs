//! Visible clock labels, driven by realtime boundaries and timezone changes.
use std::ffi::{CString, OsString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cosmic::iced::{Subscription, futures};
use ferese_config::desktop::Clock;
use jiff::{Timestamp, Zoned, tz::TimeZone};

const SECOND: i128 = 1_000_000_000;
const MINUTE: i128 = 60 * SECOND;
const UNNAMED_ZONE: &str = "ferese/unnamed-system-zone";
// Fractional formats keep the previous sampling rate; nanosecond formats must
// not turn into a nanosecond timer that continuously rebuilds the shell.
const FRACTION_SAMPLE: i128 = SECOND / 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Labels {
    pub bar: String,
    pub desktop: (String, String),
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct Format {
    time: String,
    date: Option<String>,
    zone: Option<String>,
    lowercase: bool,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct Settings {
    bar: bool,
    desktop: Option<Arc<Format>>,
}

#[derive(Default)]
pub struct Service {
    desktop: Option<Arc<Format>>,
}

impl Service {
    pub fn new(clock: &Clock) -> Self {
        let mut service = Self::default();
        service.configure(clock);
        service
    }

    pub fn configure(&mut self, clock: &Clock) {
        if !clock.enabled {
            self.desktop = None;
            return;
        }

        let date = clock.show_date.then_some(clock.date_format.as_str());
        let zone = clock.time_zone.as_deref().filter(|zone| !zone.is_empty());
        if self.desktop.as_ref().is_some_and(|format| {
            format.time == clock.time_format
                && format.date.as_deref() == date
                && format.zone.as_deref() == zone
                && format.lowercase == clock.lowercase
        }) {
            return;
        }

        self.desktop = Some(Arc::new(Format {
            time: clock.time_format.clone(),
            date: date.map(str::to_owned),
            zone: zone.map(str::to_owned),
            lowercase: clock.lowercase,
        }));
    }

    pub fn subscription(&self, bar: bool, desktop_visible: bool) -> Subscription<Labels> {
        self.settings(bar, desktop_visible)
            .map_or_else(Subscription::none, |settings| Subscription::run_with(settings, stream))
    }

    fn settings(&self, bar: bool, desktop_visible: bool) -> Option<Settings> {
        let desktop = if desktop_visible { self.desktop.clone() } else { None };
        if !bar && desktop.is_none() {
            return None;
        }

        Some(Settings { bar, desktop })
    }
}

fn stream(settings: &Settings) -> impl futures::Stream<Item = Labels> + use<> {
    let settings = settings.clone();
    let (send, receive) = tokio::sync::mpsc::channel(1);
    let stop = match UnixStream::pair() {
        Ok((stop, cancel)) => {
            match std::thread::Builder::new().name("ferese-clock".into()).spawn(move || {
                if let Err(error) = watch(&settings, send, cancel) {
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

fn watch(settings: &Settings, send: tokio::sync::mpsc::Sender<Labels>, cancel: UnixStream) -> io::Result<()> {
    let timer =
        owned_fd(unsafe { libc::timerfd_create(libc::CLOCK_REALTIME, libc::TFD_CLOEXEC | libc::TFD_NONBLOCK) })?;
    let (mut system, mut desktop, mut paths) = zones(settings);
    let mut timezone = ZoneWatch::new(&paths)?;
    let mut previous = None;
    loop {
        let now = Timestamp::now();
        let labels = labels(settings, now, &system, &desktop);
        if previous.as_ref() != Some(&labels) {
            previous = Some(labels.clone());
            if send.blocking_send(labels).is_err() {
                return Ok(());
            }
        }
        if arm(&timer, deadline(settings, now, &system, &desktop))? {
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
            (system, desktop, paths) = zones(settings);
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

fn zones(settings: &Settings) -> (TimeZone, TimeZone, Vec<PathBuf>) {
    let mut paths = vec![PathBuf::from("/etc/localtime"), PathBuf::from("/etc/timezone")];
    // Load the file directly: Jiff's system timezone cache lasts five minutes.
    let system = match std::env::var("TZ") {
        Ok(zone) if zone.is_empty() => TimeZone::UTC,
        Ok(zone) => named_zone(zone.trim_start_matches(':'), &mut paths).unwrap_or_else(TimeZone::system),
        Err(_) => file_zone(Path::new("/etc/localtime"), &mut paths).unwrap_or_else(TimeZone::system),
    };
    let desktop = settings
        .desktop
        .as_ref()
        .and_then(|format| format.zone.as_ref())
        .and_then(|zone| named_zone(zone, &mut paths))
        .unwrap_or_else(|| system.clone());
    (system, desktop, paths)
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

fn labels(settings: &Settings, now: Timestamp, system: &TimeZone, desktop: &TimeZone) -> Labels {
    let bar = if settings.bar {
        ferese_theme::calendar::format_bar_time(&now.to_zoned(system.clone()))
    } else {
        String::new()
    };
    let desktop = settings
        .desktop
        .as_ref()
        .map(|format| {
            let now = now.to_zoned(desktop.clone());
            let mut broken = jiff::fmt::strtime::BrokenDownTime::from(&now);
            if desktop.iana_name() == Some(UNNAMED_ZONE) {
                // Copied localtime files have no IANA name. Preserve Jiff's
                // %Q offset fallback while retaining their DST/abbreviation.
                broken.set_iana_time_zone(None);
            }
            let time = broken.to_string(&format.time).unwrap_or_default();
            let date = format
                .date
                .as_ref()
                .map(|date| broken.to_string(date).unwrap_or_default())
                .unwrap_or_default();
            if format.lowercase {
                (time.to_lowercase(), date.to_lowercase())
            } else {
                (time, date)
            }
        })
        .unwrap_or_default();
    Labels { bar, desktop }
}

fn deadline(settings: &Settings, now: Timestamp, system: &TimeZone, desktop: &TimeZone) -> i128 {
    let mut next = i128::MAX;
    if settings.bar {
        next = next.min(zone_deadline(&now.to_zoned(system.clone()), MINUTE));
    }
    if let Some(format) = &settings.desktop {
        let precision =
            format_precision(&format.time).min(format.date.as_deref().map(format_precision).unwrap_or(MINUTE));
        next = next.min(zone_deadline(&now.to_zoned(desktop.clone()), precision));
    }
    next
}

fn zone_deadline(now: &Zoned, precision: i128) -> i128 {
    let stamp = now.timestamp().as_nanosecond();
    let next = if precision == MINUTE {
        stamp + MINUTE - i128::from(now.second()) * SECOND - i128::from(now.subsec_nanosecond())
    } else {
        (stamp.div_euclid(precision) + 1) * precision
    };
    now.time_zone()
        .following(now.timestamp())
        .next()
        .map_or(next, |transition| next.min(transition.timestamp().as_nanosecond()))
}

fn format_precision(format: &str) -> i128 {
    let mut precision = MINUTE;
    let mut chars = format.chars();
    while let Some(character) = chars.next() {
        if character != '%' {
            continue;
        }
        let Some(mut directive) = chars.next() else {
            break;
        };
        if directive == '%' {
            continue;
        }
        while matches!(directive, '_' | '-' | '0' | '^' | '#' | ':' | '.') || directive.is_ascii_digit() {
            let Some(next) = chars.next() else {
                return precision;
            };
            directive = next;
        }
        precision = precision.min(match directive {
            'f' | 'N' => FRACTION_SAMPLE,
            'S' | 's' | 'T' | 'c' | 'r' | 'X' => SECOND,
            _ => MINUTE,
        });
    }
    precision
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

    fn settings(time: Option<&str>, date: Option<&str>) -> Settings {
        Settings {
            bar: true,
            desktop: time.map(|time| {
                Arc::new(Format {
                    time: time.into(),
                    date: date.map(str::to_owned),
                    zone: None,
                    lowercase: false,
                })
            }),
        }
    }

    #[test]
    fn subscription_rebuilds_and_style_changes_keep_the_cached_format() {
        let mut clock = Clock {
            enabled: true,
            time_zone: Some("Asia/Kathmandu".into()),
            ..Clock::default()
        };
        let mut service = Service::new(&clock);
        let first = service.settings(true, true).unwrap();
        let format = first.desktop.as_ref().unwrap();
        for _ in 0..100 {
            let current = service.settings(true, true).unwrap();
            assert!(Arc::ptr_eq(format, current.desktop.as_ref().unwrap()));
            assert_eq!(first, current);
        }

        clock.time_size += 8.;
        clock.margin_x += 10;
        service.configure(&clock);
        assert!(Arc::ptr_eq(format, service.desktop.as_ref().unwrap()));
        assert!(service.settings(true, false).unwrap().desktop.is_none());
        assert!(service.settings(false, false).is_none());
        assert!(Arc::ptr_eq(
            format,
            service.settings(false, true).unwrap().desktop.as_ref().unwrap()
        ));
    }

    #[test]
    fn format_changes_replace_subscription_identity_without_mutating_the_worker_settings() {
        let mut clock = Clock {
            enabled: true,
            ..Clock::default()
        };
        let mut service = Service::new(&clock);
        let initial = service.settings(true, true).unwrap();
        clock.time_format = "%H:%M:%S".into();
        service.configure(&clock);
        let seconds = service.settings(true, true).unwrap();
        assert_ne!(initial, seconds);
        assert_eq!(initial.desktop.as_ref().unwrap().time, "%-I:%M %p");

        clock.show_date = false;
        service.configure(&clock);
        let no_date = service.settings(true, true).unwrap();
        assert_ne!(seconds, no_date);
        assert!(no_date.desktop.as_ref().unwrap().date.is_none());
        clock.date_format = "%Y".into();
        service.configure(&clock);
        assert!(Arc::ptr_eq(
            no_date.desktop.as_ref().unwrap(),
            service.desktop.as_ref().unwrap()
        ));

        clock.time_zone = Some("UTC".into());
        service.configure(&clock);
        assert_ne!(no_date, service.settings(true, true).unwrap());
        clock.lowercase = !clock.lowercase;
        let previous = service.settings(true, true).unwrap();
        service.configure(&clock);
        assert_ne!(previous, service.settings(true, true).unwrap());
        clock.enabled = false;
        service.configure(&clock);
        assert!(service.settings(false, true).is_none());
        assert!(service.settings(true, true).unwrap().desktop.is_none());
        clock.enabled = true;
        service.configure(&clock);
        assert!(service.settings(false, true).unwrap().desktop.is_some());
    }

    #[test]
    fn production_subscription_uses_visible_outputs_formats_and_hidden_date() {
        let mut clock = Clock {
            enabled: true,
            time_format: "%H:%M".into(),
            date_format: "%S%.f".into(),
            ..Clock::default()
        };
        assert_eq!(Service::new(&clock).subscription(false, false).units(), 0);
        assert_eq!(Service::new(&clock).subscription(true, false).units(), 1);
        let hidden = Service::new(&clock).settings(true, false).unwrap();
        assert!(hidden.desktop.is_none());
        let now: Timestamp = "2026-10-04T12:34:56.123Z".parse().unwrap();
        assert_eq!(
            deadline(&hidden, now, &TimeZone::UTC, &TimeZone::UTC),
            "2026-10-04T12:35:00Z".parse::<Timestamp>().unwrap().as_nanosecond()
        );
        let visible = Service::new(&clock).settings(true, true).unwrap();
        assert_eq!(
            deadline(&visible, now, &TimeZone::UTC, &TimeZone::UTC),
            "2026-10-04T12:34:56.5Z".parse::<Timestamp>().unwrap().as_nanosecond()
        );
        clock.show_date = false;
        let hidden_date = Service::new(&clock).settings(true, true).unwrap();
        assert_ne!(hidden_date, visible);
        assert_eq!(
            deadline(&hidden_date, now, &TimeZone::UTC, &TimeZone::UTC),
            "2026-10-04T12:35:00Z".parse::<Timestamp>().unwrap().as_nanosecond()
        );
        clock.time_format = "%S".into();
        assert_ne!(Service::new(&clock).settings(true, true).unwrap(), hidden_date);
        clock.enabled = false;
        assert_eq!(Service::new(&clock).subscription(false, true).units(), 0);
    }

    #[test]
    fn named_zone_labels_match_configured_clock_including_aliases_and_case() {
        let now: Timestamp = "2026-10-04T12:34:56.123Z".parse().unwrap();
        for zone in ["America/New_York", "Etc/UTC", "Asia/Kathmandu", "UTC", "utc"] {
            let clock = Clock {
                enabled: true,
                time_format: "%H:%M:%S%.3f %Q %Z".into(),
                date_format: "%a %F".into(),
                time_zone: Some(zone.into()),
                ..Clock::default()
            };
            let settings = Service::new(&clock).settings(true, true).unwrap();
            let desktop = named_zone(zone, &mut Vec::new()).unwrap_or_else(|| panic!("zone {zone} unavailable"));
            let actual = labels(&settings, now, &TimeZone::UTC, &desktop).desktop;
            assert_eq!(actual, clock.labels(&now.to_zoned(TimeZone::UTC)).unwrap(), "{zone}");
        }
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
        let bar_settings = settings(None, None);
        assert_eq!(labels(&bar_settings, now, &first, &first).bar, "4 oct, 8:34 am");
        assert_eq!(labels(&bar_settings, now, &second, &second).bar, "4 oct, 6:19 pm");
        let settings = settings(Some("%Q %:Q %Z %z"), None);
        assert_eq!(
            labels(&settings, now, &first, &first).desktop.0,
            "-0400 -04:00 EDT -0400"
        );
        assert_eq!(
            labels(&settings, now, &second, &second).desktop.0,
            "+0545 +05:45 +0545 +0545"
        );
    }

    #[test]
    fn visible_format_precision_handles_seconds_escapes_and_sampled_fractions() {
        for format in ["%-I:%M %p", "%a, %b %-d", "%%S %%f", "%:z %:::z", "fixed text"] {
            assert_eq!(format_precision(format), MINUTE, "{format}");
        }
        for format in ["%S", "%_3S", "%s", "%T", "%c", "%r", "%X"] {
            assert_eq!(format_precision(format), SECOND, "{format}");
        }
        for format in ["%f", "%N", "%.f", "%.3f", "%9f", "%1f"] {
            assert_eq!(format_precision(format), FRACTION_SAMPLE, "{format}");
        }
        let now: Timestamp = "2026-10-04T12:34:56.123Z".parse().unwrap();
        let minute = "2026-10-04T12:35:00Z".parse::<Timestamp>().unwrap().as_nanosecond();
        assert_eq!(
            deadline(&settings(None, None), now, &TimeZone::UTC, &TimeZone::UTC),
            minute
        );
        assert_eq!(
            deadline(
                &settings(Some("%H:%M"), Some("%S")),
                now,
                &TimeZone::UTC,
                &TimeZone::UTC
            ),
            "2026-10-04T12:34:57Z".parse::<Timestamp>().unwrap().as_nanosecond()
        );
        assert_eq!(
            deadline(
                &settings(Some("%H:%M:%S%.3f"), None),
                now,
                &TimeZone::UTC,
                &TimeZone::UTC
            ),
            "2026-10-04T12:34:56.5Z".parse::<Timestamp>().unwrap().as_nanosecond()
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
                zone_deadline(&now.to_zoned(TimeZone::UTC), MINUTE),
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
            assert_eq!(
                zone_deadline(&now.to_zoned(zone.clone()), MINUTE),
                expected.as_nanosecond()
            );
            assert_eq!(labels(&settings(None, None), expected, &zone, &zone).bar, label);
        }
        let zone = TimeZone::fixed(jiff::tz::Offset::from_seconds(30).unwrap());
        let now: Timestamp = "2026-10-04T12:34:20Z".parse().unwrap();
        assert_eq!(
            zone_deadline(&now.to_zoned(zone), MINUTE),
            "2026-10-04T12:34:30Z".parse::<Timestamp>().unwrap().as_nanosecond()
        );
    }

    #[test]
    fn realtime_jumps_recompute_boundaries_and_timezone_labels() {
        let settings = settings(Some("%H:%M %Z"), None);
        let initial: Timestamp = "2026-10-04T12:34:56Z".parse().unwrap();
        let earlier: Timestamp = "2026-10-04T11:20:01Z".parse().unwrap();
        let later: Timestamp = "2026-10-04T15:40:01Z".parse().unwrap();
        let first = labels(&settings, initial, &TimeZone::UTC, &TimeZone::UTC);
        for (now, expected) in [(earlier, "2026-10-04T11:21:00Z"), (later, "2026-10-04T15:41:00Z")] {
            assert_ne!(labels(&settings, now, &TimeZone::UTC, &TimeZone::UTC), first);
            assert_eq!(
                deadline(&settings, now, &TimeZone::UTC, &TimeZone::UTC),
                expected.parse::<Timestamp>().unwrap().as_nanosecond()
            );
        }
        let zone = TimeZone::get("Asia/Kathmandu").unwrap();
        assert_ne!(labels(&settings, initial, &zone, &zone), first);
        assert_eq!(labels(&settings, initial, &zone, &zone).desktop.0, "18:19 +0545");
        assert_eq!(
            labels(&settings, initial, &TimeZone::UTC, &TimeZone::UTC),
            labels(
                &settings,
                initial + jiff::SignedDuration::from_secs(1),
                &TimeZone::UTC,
                &TimeZone::UTC
            )
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
            done_send.send(watch(&settings(None, None), send, cancel)).unwrap();
        });
        assert!(receive.blocking_recv().is_some());
        drop(stop);
        done_receive.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
        assert!(receive.blocking_recv().is_none());
    }
}
