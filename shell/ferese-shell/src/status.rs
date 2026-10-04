//! Bounded, off-UI-thread adapters. A missing service is `None`, never a fake state.
mod audio_cache;
mod bluetooth;
mod dbus_cache;
mod hardware;
mod network;

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};
use std::{env, fs, thread};

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub network: Option<Network>,
    pub bluetooth: Option<Bluetooth>,
    pub audio: Option<Audio>,
    pub battery: Option<Battery>,
    pub power_profiles: Option<PowerProfiles>,
    pub brightness: Option<u8>,
    pub notifications: Option<Notifications>,
    pub poweroff: bool,
    pub reboot: bool,
    pub suspend: bool,
}

#[derive(Clone, Debug)]
pub struct Network {
    pub enabled: bool,
    pub connection: Option<String>,
    pub signal: u8,
}

#[derive(Clone, Debug)]
pub struct Bluetooth {
    pub enabled: bool,
    pub devices: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Audio {
    pub volume: u8,
    pub muted: bool,
    pub output: String,
}

#[derive(Clone, Debug)]
pub struct Battery {
    pub percent: u8,
    pub status: String,
}

#[derive(Clone, Debug)]
pub struct PowerProfiles {
    pub active: String,
    pub available: [bool; 3],
}

#[derive(Clone, Debug)]
pub struct Notifications {
    pub count: u32,
    pub dnd: bool,
}

#[derive(Clone, Debug)]
pub enum Action {
    Wifi(bool),
    Bluetooth(bool),
    Volume(u8),
    Mute(bool),
    Brightness(u8),
    Dnd(bool),
    Notifications,
    Settings,
    Poweroff,
    Reboot,
    Suspend,
    PowerProfile(&'static str),
}

impl Action {
    fn key(&self) -> std::mem::Discriminant<Self> {
        std::mem::discriminant(self)
    }
}

#[derive(Clone, Debug)]
pub struct ActionError {
    pub action: Action,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct Update {
    pub snapshot: Snapshot,
    pub generation: u64,
    pub error: Option<ActionError>,
}

pub struct Service {
    settings: Arc<Mutex<Option<Vec<String>>>>,
    tx: SyncSender<(u64, Action)>,
    updates: Updates,
    pub generation: u64,
}

// The receiver stays with the service so a subscription can start after the
// first poll. A watch channel retains the newest result without blocking workers.
#[derive(Clone)]
struct Updates(Arc<tokio::sync::watch::Receiver<Option<Update>>>);

impl std::hash::Hash for Updates {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::ptr::hash(Arc::as_ptr(&self.0), state);
    }
}

impl Updates {
    fn stream(&self) -> impl cosmic::iced::futures::Stream<Item = Update> + use<> {
        cosmic::iced::futures::stream::unfold(self.0.as_ref().clone(), |mut receiver| async move {
            loop {
                receiver.changed().await.ok()?;
                let update = receiver.borrow_and_update().clone();
                if let Some(update) = update {
                    return Some((update, receiver));
                }
            }
        })
    }
}

type PollState = (Mutex<(u64, bool, Option<ActionError>, bool, u64)>, Condvar);

fn wait_for_poll(shared: &PollState) -> Option<u64> {
    let state = shared
        .1
        .wait_while(shared.0.lock().unwrap(), |state| state.1 && !state.3)
        .unwrap();
    (!state.3).then_some(state.0)
}

fn wake_status(shared: &PollState) {
    let mut state = shared.0.lock().unwrap_or_else(|error| error.into_inner());
    state.4 = state.4.wrapping_add(1);
    shared.1.notify_one();
}

impl Service {
    pub fn start(settings: Option<Vec<String>>) -> Self {
        let live_settings = Arc::new(Mutex::new(settings));
        let settings = live_settings.clone();
        let (tx, commands) = mpsc::sync_channel::<(u64, Action)>(64);
        let (updates, rx) = tokio::sync::watch::channel(None);
        // Polling and writes have separate workers: a missing D-Bus service must
        // never hold up volume/brightness changes. Publish only coherent polls.
        let shared = Arc::new((Mutex::new((0u64, false, None::<ActionError>, false, 0)), Condvar::new()));
        let polling = shared.clone();

        thread::spawn(move || {
            let mut network = dbus_cache::Cache::with_wake("org.freedesktop.NetworkManager", polling.clone());
            let mut bluetooth = dbus_cache::Cache::with_wake("org.bluez", polling.clone());
            let mut battery_cache = dbus_cache::Cache::with_wake("org.freedesktop.UPower", polling.clone());
            let mut profiles = dbus_cache::Cache::with_wake("org.freedesktop.UPower.PowerProfiles", polling.clone());
            let mut legacy_profiles = dbus_cache::Cache::with_wake("net.hadess.PowerProfiles", polling.clone());
            let mut login = dbus_cache::Cache::with_wake("org.freedesktop.login1", polling.clone());
            let mut audio = audio_cache::Cache::new(polling.clone());
            let mut hardware = hardware::Monitor::start(polling.clone()).ok();

            while let Some(before) = wait_for_poll(&polling) {
                let revision = polling.0.lock().unwrap().4;
                if hardware.as_ref().is_none_or(|monitor| !monitor.alive()) {
                    hardware = hardware::Monitor::start(polling.clone()).ok();
                }
                let capabilities = login.read(before, read_capabilities).unwrap_or_default();
                let primary_profiles = profiles.read(before, |connection| {
                    read_power_profiles(
                        connection,
                        "org.freedesktop.UPower.PowerProfiles",
                        "/org/freedesktop/UPower/PowerProfiles",
                    )
                    .map(Some)
                });
                let fallback_profiles = legacy_profiles.read(before, |connection| {
                    read_power_profiles(connection, "net.hadess.PowerProfiles", "/net/hadess/PowerProfiles").map(Some)
                });

                let mut snapshot = Snapshot {
                    battery: battery_cache.read(before, read_battery).or_else(battery),
                    power_profiles: primary_profiles.or(fallback_profiles),
                    brightness: brightness(),
                    poweroff: capabilities.0,
                    reboot: capabilities.1,
                    suspend: capabilities.2,
                    ..Snapshot::default()
                };
                snapshot.audio = audio.read(before);
                snapshot.network = network.read(before, network::read);
                snapshot.bluetooth = bluetooth.read(before, bluetooth::read);
                let state = polling.0.lock().unwrap();

                if state.3 {
                    break;
                }

                if state.0 == before && !state.1 {
                    updates.send_replace(Some(Update {
                        snapshot,
                        generation: state.0,
                        error: state.2.clone(),
                    }));
                    let retry = [
                        network.next_retry(),
                        bluetooth.next_retry(),
                        battery_cache.next_retry(),
                        profiles.next_retry(),
                        legacy_profiles.next_retry(),
                        login.next_retry(),
                        audio.next_retry(),
                        hardware
                            .as_ref()
                            .is_none_or(|monitor| !monitor.alive())
                            .then(|| Instant::now() + Duration::from_secs(5)),
                    ]
                    .into_iter()
                    .flatten()
                    .min();
                    let unchanged = |state: &mut (u64, bool, Option<ActionError>, bool, u64)| {
                        !state.3 && state.0 == before && state.4 == revision
                    };
                    if !audio.changed() {
                        if let Some(deadline) = retry {
                            let _ = polling.1.wait_timeout_while(
                                state,
                                deadline.saturating_duration_since(Instant::now()),
                                unchanged,
                            );
                        } else {
                            drop(polling.1.wait_while(state, unchanged));
                        }
                    }
                }
            }
        });

        thread::spawn(move || {
            while let Ok(first) = commands.recv() {
                let mut pending = vec![first];

                for command in commands.try_iter() {
                    pending.retain(|(_, action)| action.key() != command.1.key());
                    pending.push(command);
                }

                for (id, action) in pending {
                    {
                        let mut state = shared.0.lock().unwrap();
                        state.0 = id;
                        state.1 = true;
                    }
                    let settings = settings.lock().unwrap().clone();
                    let error = execute(&action, settings.as_deref())
                        .err()
                        .map(|message| ActionError { action, message });
                    let mut state = shared.0.lock().unwrap();
                    state.1 = false;
                    state.2 = error;
                    state.4 = state.4.wrapping_add(1);
                    shared.1.notify_one();
                }
            }

            shared.0.lock().unwrap().3 = true;
            shared.1.notify_one();
        });

        Self {
            settings: live_settings,
            tx,
            updates: Updates(Arc::new(rx)),
            generation: 0,
        }
    }

    pub fn send(&mut self, action: Action) -> Result<(), ActionError> {
        let generation = self.generation + 1;
        let error_action = action.clone();
        self.tx.try_send((generation, action)).map_err(|_| ActionError {
            action: error_action,
            message: "Controls are busy; please try again".to_owned(),
        })?;
        self.generation = generation;

        Ok(())
    }

    pub fn update_settings(&self, settings: Option<Vec<String>>) {
        *self.settings.lock().unwrap() = settings;
    }

    pub fn subscription(&self) -> cosmic::iced::Subscription<Update> {
        cosmic::iced::Subscription::run_with(self.updates.clone(), Updates::stream)
    }
}

pub fn available(program: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let executable = |p: &Path| {
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };

    if program.contains('/') {
        return executable(Path::new(program));
    }

    env::var_os("PATH").is_some_and(|paths| env::split_paths(&paths).any(|path| executable(&path.join(program))))
}

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    run_with_timeout(program, args, "2s")
}

fn run_with_timeout(program: &str, args: &[&str], timeout: &str) -> Result<String, String> {
    if !available(program) {
        return Err(format!("{program} is not installed"));
    }

    // Coreutils timeout bounds disconnected D-Bus services, too. Never invoke a shell.
    let output = Command::new("timeout")
        .args(["--kill-after=1s", timeout, program])
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr)
            .trim()
            .chars()
            .take(180)
            .collect::<String>();
        return Err(if message.is_empty() {
            format!("{program} did not complete successfully")
        } else {
            message
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

// Connections live only on the polling worker. Each method still runs on every
// poll with a bounded reply timeout; capability values are never cached.
struct StatusBus {
    system: bool,
    connection: Option<zbus::blocking::Connection>,
}

impl StatusBus {
    fn new(system: bool) -> Self {
        Self {
            system,
            connection: None,
        }
    }

    fn query<T>(&mut self, query: impl FnOnce(&zbus::blocking::Connection) -> zbus::Result<T>) -> Option<T> {
        if self.connection.is_none() {
            let builder = if self.system {
                zbus::blocking::connection::Builder::system()
            } else {
                zbus::blocking::connection::Builder::session()
            };

            self.connection = builder.ok()?.method_timeout(Duration::from_secs(2)).build().ok();
        }

        match query(self.connection.as_ref()?) {
            Ok(value) => Some(value),
            Err(_) => {
                // A disconnected/restarted bus must be rediscovered next poll.
                self.connection = None;
                None
            }
        }
    }

    #[cfg(test)]
    fn can_power(&mut self, method: &str) -> bool {
        self.query(|connection| {
            connection
                .call_method(
                    Some("org.freedesktop.login1"),
                    "/org/freedesktop/login1",
                    Some("org.freedesktop.login1.Manager"),
                    method,
                    &(),
                )?
                .body()
                .deserialize::<String>()
        })
        .is_some_and(|value| value == "yes" || value == "challenge")
    }

    #[cfg(test)]
    fn notification_service_owned(&mut self) -> bool {
        self.query(|connection| {
            connection
                .call_method(
                    Some("org.freedesktop.DBus"),
                    "/org/freedesktop/DBus",
                    Some("org.freedesktop.DBus"),
                    "NameHasOwner",
                    &("org.erikreider.swaync",),
                )?
                .body()
                .deserialize::<bool>()
        })
        .unwrap_or(false)
    }
}

fn read_capabilities(connection: &zbus::blocking::Connection) -> zbus::Result<Option<(bool, bool, bool)>> {
    let can = |method| -> zbus::Result<bool> {
        let value: String = connection
            .call_method(
                Some("org.freedesktop.login1"),
                "/org/freedesktop/login1",
                Some("org.freedesktop.login1.Manager"),
                method,
                &(),
            )?
            .body()
            .deserialize()?;

        Ok(value == "yes" || value == "challenge")
    };

    Ok(Some((can("CanPowerOff")?, can("CanReboot")?, can("CanSuspend")?)))
}

fn read_battery(connection: &zbus::blocking::Connection) -> zbus::Result<Option<Battery>> {
    let proxy = zbus::blocking::Proxy::new(
        connection,
        "org.freedesktop.UPower",
        "/org/freedesktop/UPower/devices/DisplayDevice",
        "org.freedesktop.UPower.Device",
    )?;
    if !proxy.get_property::<bool>("IsPresent")? {
        return Ok(None);
    }
    let percent = proxy.get_property::<f64>("Percentage")?;
    let status = match proxy.get_property::<u32>("State")? {
        1 => "Charging",
        2 => "Discharging",
        3 => "Empty",
        4 => "Full",
        5 => "Pending charge",
        6 => "Pending discharge",
        _ => "Unknown",
    };
    Ok(Some(Battery {
        percent: percent.round().clamp(0., 100.) as u8,
        status: status.into(),
    }))
}

pub fn parse_audio(value: &str) -> Option<(u8, bool)> {
    let volume = value
        .strip_prefix("Volume: ")?
        .split_whitespace()
        .next()?
        .parse::<f32>()
        .ok()?;

    if !volume.is_finite() || volume < 0.0 {
        return None;
    }

    Some((
        (volume * 100.0).round().clamp(0.0, 100.0) as u8,
        value.contains("[MUTED]"),
    ))
}

fn audio() -> Option<Audio> {
    let (volume, muted) = parse_audio(&run("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"]).ok()?)?;
    let info = run("wpctl", &["inspect", "@DEFAULT_AUDIO_SINK@"]).ok()?;
    let output = info
        .lines()
        .find_map(|line| line.trim().strip_prefix("node.description = "))
        .map(|s| s.trim_matches('"').to_owned())
        .unwrap_or_else(|| "Default output".into());

    Some(Audio { volume, muted, output })
}

fn read(path: &Path, name: &str) -> Option<String> {
    fs::read_to_string(path.join(name)).ok().map(|s| s.trim().to_owned())
}

fn battery() -> Option<Battery> {
    let entries = fs::read_dir("/sys/class/power_supply").ok()?;
    // Prefer a system battery, not a mouse/headset battery.
    let mut batteries = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| read(p, "type").as_deref() == Some("Battery") && read(p, "scope").as_deref() != Some("Device"))
        .collect::<Vec<_>>();

    batteries.sort();
    batteries.iter().find_map(|path| {
        Some(Battery {
            percent: read(path, "capacity")?.parse::<u8>().ok()?.min(100),
            status: read(path, "status")?,
        })
    })
}

#[cfg(test)]
fn power_profiles(bus: &mut StatusBus) -> Option<PowerProfiles> {
    bus.query(|connection| {
        read_power_profiles(
            connection,
            "org.freedesktop.UPower.PowerProfiles",
            "/org/freedesktop/UPower/PowerProfiles",
        )
        .or_else(|_| read_power_profiles(connection, "net.hadess.PowerProfiles", "/net/hadess/PowerProfiles"))
    })
}

fn read_power_profiles(
    connection: &zbus::blocking::Connection,
    destination: &str,
    path: &str,
) -> zbus::Result<PowerProfiles> {
    let proxy = zbus::blocking::Proxy::new(connection, destination, path, destination)?;
    let active = proxy.get_property::<String>("ActiveProfile")?;
    let profiles =
        proxy.get_property::<Vec<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>>("Profiles")?;
    let names = ["power-saver", "balanced", "performance"];
    let available = names.map(|name| {
        profiles.iter().any(|profile| {
            profile
                .get("Profile")
                .and_then(|value| String::try_from(value.clone()).ok())
                .is_some_and(|value| value == name)
        })
    });
    Ok(PowerProfiles { active, available })
}

fn brightness() -> Option<u8> {
    let mut devices = fs::read_dir("/sys/class/backlight")
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    devices.sort();
    devices.iter().find_map(|path| {
        let current = read(path, "brightness")?.parse::<f64>().ok()?;
        let max = read(path, "max_brightness")?.parse::<f64>().ok()?;
        brightness_percent(current, max)
    })
}

fn brightness_percent(current: f64, max: f64) -> Option<u8> {
    (max.is_finite() && max > 0.0 && current.is_finite() && current >= 0.0)
        .then(|| (100.0 * current / max).round().clamp(0.0, 100.0) as u8)
}

pub(super) fn execute_power(action: Action, force: bool) -> Result<(), String> {
    let action = match action {
        Action::Poweroff => "poweroff",
        Action::Reboot => "reboot",
        Action::Suspend => "suspend",
        _ => return Err("Invalid power action".to_owned()),
    };
    let check = if force {
        "--check-inhibitors=no"
    } else {
        "--check-inhibitors=yes"
    };
    run_with_timeout("systemctl", &[check, action], "120s").map(|_| ())
}

fn execute(action: &Action, settings: Option<&[String]>) -> Result<(), String> {
    match action {
        Action::Wifi(on) => run("nmcli", &["radio", "wifi", if *on { "on" } else { "off" }]),
        Action::Bluetooth(on) => run("bluetoothctl", &["power", if *on { "on" } else { "off" }]),
        Action::Volume(value) => run(
            "wpctl",
            &[
                "set-volume",
                "-l",
                "1.0",
                "@DEFAULT_AUDIO_SINK@",
                &format!("{}%", value.min(&100)),
            ],
        ),
        Action::Mute(on) => run(
            "wpctl",
            &["set-mute", "@DEFAULT_AUDIO_SINK@", if *on { "1" } else { "0" }],
        ),
        Action::Brightness(value) => run(
            "brightnessctl",
            &["--class=backlight", "set", &format!("{}%", value.clamp(&1, &100))],
        ),
        Action::Dnd(_) | Action::Notifications => return Err("Native notifications are unavailable".into()),
        Action::PowerProfile(profile) if matches!(*profile, "power-saver" | "balanced" | "performance") => {
            run("powerprofilesctl", &["set", profile])
        }
        Action::PowerProfile(_) => return Err("Invalid power profile".to_owned()),
        Action::Poweroff => run("systemctl", &["poweroff"]),
        Action::Reboot => run("systemctl", &["reboot"]),
        Action::Suspend => run("systemctl", &["suspend"]),
        Action::Settings => {
            let argv = settings
                .filter(|argv| !argv.is_empty())
                .ok_or("No settings application configured")?;
            let mut command = Command::new(&argv[0]);
            command
                .args(&argv[1..])
                .stdin(Stdio::null())
                .env_remove("WAYLAND_SOCKET")
                .env_remove("FERESE_SHELL_CONTROL_SOCKET");
            if let Some(display) = env::var_os("FERESE_PUBLIC_WAYLAND_DISPLAY") {
                command.env("WAYLAND_DISPLAY", display);
            }
            command.env_remove("FERESE_PUBLIC_WAYLAND_DISPLAY");
            crate::renderer::configure_app(&mut command);
            let mut child = command.spawn().map_err(|e| e.to_string())?;
            thread::spawn(move || {
                let _ = child.wait();
            });
            return Ok(());
        }
    }
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) struct TestBus {
        daemon: std::process::Child,

        pub(super) address: String,
    }

    impl TestBus {
        pub(super) fn new() -> Self {
            use std::io::{BufRead, BufReader};
            let mut daemon = Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            let mut address = String::new();

            BufReader::new(daemon.stdout.take().unwrap())
                .read_line(&mut address)
                .unwrap();

            Self {
                daemon,
                address: address.trim().to_owned(),
            }
        }

        pub(super) fn connect(&self) -> zbus::blocking::Connection {
            zbus::blocking::connection::Builder::address(self.address.as_str())
                .unwrap()
                .method_timeout(Duration::from_secs(2))
                .build()
                .unwrap()
        }
    }

    impl Drop for TestBus {
        fn drop(&mut self) {
            let _ = self.daemon.kill();
            let _ = self.daemon.wait();
        }
    }

    #[test]
    #[ignore = "requires a running Power Profiles D-Bus service"]
    fn reads_available_power_profiles_from_system_bus() {
        let mut bus = StatusBus::new(true);
        let profiles = power_profiles(&mut bus).expect("Power Profiles service is available");
        let names = ["power-saver", "balanced", "performance"];
        assert!(
            names
                .iter()
                .zip(profiles.available)
                .any(|(name, available)| { available && profiles.active == *name })
        );
    }

    struct MockLogin1(Arc<std::sync::atomic::AtomicU8>);

    #[zbus::interface(name = "org.freedesktop.login1.Manager")]
    impl MockLogin1 {
        fn can_power_off(&self) -> zbus::fdo::Result<String> {
            use std::sync::atomic::Ordering;
            match self.0.load(Ordering::Relaxed) {
                0 => Ok("yes".into()),
                1 => Ok("no".into()),
                3 => {
                    thread::sleep(Duration::from_millis(500));
                    Ok("yes".into())
                }
                _ => Err(zbus::fdo::Error::Failed("test failure".into())),
            }
        }
        fn can_reboot(&self) -> String {
            "challenge".into()
        }
        fn can_suspend(&self) -> String {
            "na".into()
        }
    }

    #[test]
    #[ignore = "requires dbus-daemon; uses a private test bus"]
    fn native_bus_queries_track_live_changes_and_fail_closed() {
        use std::io::{BufRead, BufReader};
        use std::sync::atomic::{AtomicU8, Ordering};
        struct Daemon(std::process::Child);
        impl Drop for Daemon {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut daemon = Daemon(
            Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut address = String::new();
        BufReader::new(daemon.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let mode = Arc::new(AtomicU8::new(0));
        let server = zbus::blocking::connection::Builder::address(address.trim())
            .unwrap()
            .name("org.freedesktop.login1")
            .unwrap()
            .serve_at("/org/freedesktop/login1", MockLogin1(mode.clone()))
            .unwrap()
            .build()
            .unwrap();
        let connect = || {
            zbus::blocking::connection::Builder::address(address.trim())
                .unwrap()
                .method_timeout(Duration::from_millis(200))
                .build()
                .unwrap()
        };
        let mut bus = StatusBus {
            system: false,
            connection: Some(connect()),
        };
        assert!(bus.can_power("CanPowerOff"));
        assert!(bus.can_power("CanReboot"));
        assert!(!bus.can_power("CanSuspend"));
        mode.store(1, Ordering::Relaxed);
        assert!(!bus.can_power("CanPowerOff"), "capabilities must not be cached");
        assert!(bus.connection.is_some());
        assert!(!bus.notification_service_owned());
        server.request_name("org.erikreider.swaync").unwrap();
        assert!(bus.notification_service_owned());
        server.release_name("org.erikreider.swaync").unwrap();
        assert!(!bus.notification_service_owned());
        mode.store(2, Ordering::Relaxed);
        assert!(!bus.can_power("CanPowerOff"));
        assert!(
            bus.connection.is_none(),
            "failed connections must be eligible for reconnection"
        );
        bus.connection = Some(connect());
        assert!(!bus.can_power("MissingMethod"));
        assert!(bus.connection.is_none());
        bus.connection = Some(connect());
        mode.store(3, Ordering::Relaxed);
        assert!(
            !bus.can_power("CanPowerOff"),
            "a reply beyond the method deadline must fail closed"
        );
        assert!(bus.connection.is_none());
    }

    #[test]
    fn brightness_ratio_rounds_and_rejects_invalid_values() {
        assert_eq!(brightness_percent(72.0, 400.0), Some(18));
        assert_eq!(brightness_percent(2.0, 3.0), Some(67));
        assert_eq!(brightness_percent(0.0, 400.0), Some(0));

        for (current, max) in [(1.0, 0.0), (-1.0, 100.0), (f64::NAN, 100.0), (1.0, f64::INFINITY)] {
            assert_eq!(brightness_percent(current, max), None);
        }
    }

    #[test]
    fn status_stream_retains_latest_result_wakes_and_closes() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::task::{Context, Poll};

        use cosmic::iced::futures::Stream;
        use cosmic::iced::futures::task::{ArcWake, waker};

        #[derive(Default)]
        struct WakeCount(AtomicUsize);
        impl ArcWake for WakeCount {
            fn wake_by_ref(arc_self: &Arc<Self>) {
                arc_self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let result = |generation| {
            Some(Update {
                snapshot: Snapshot {
                    brightness: Some(42),
                    ..Snapshot::default()
                },
                generation,
                error: Some(ActionError {
                    action: Action::Bluetooth(true),
                    message: "service unavailable".into(),
                }),
            })
        };
        let (sender, receiver) = tokio::sync::watch::channel(None);
        let updates = Updates(Arc::new(receiver));
        // Results produced before the UI subscribes remain available; a slow
        // consumer gets the newest generation instead of a stale queued result.
        sender.send_replace(result(1));
        sender.send_replace(result(2));
        let mut stream = Box::pin(updates.stream());
        let wakes = Arc::new(WakeCount::default());
        let waker = waker(wakes.clone());
        let mut context = Context::from_waker(&waker);
        let Poll::Ready(Some(update)) = stream.as_mut().poll_next(&mut context) else {
            panic!("startup result missing");
        };
        assert_eq!(update.generation, 2);
        assert_eq!(update.snapshot.brightness, Some(42));
        let error = update.error.as_ref().unwrap();
        assert_eq!(error.message, "service unavailable");
        assert!(matches!(error.action, Action::Bluetooth(true)));
        assert!(stream.as_mut().poll_next(&mut context).is_pending());
        let before = wakes.0.load(Ordering::Relaxed);
        thread::spawn(move || {
            sender.send_replace(result(3));
            // Closing still delivers the last unseen update before ending.
        })
        .join()
        .unwrap();
        assert!(wakes.0.load(Ordering::Relaxed) > before);
        let Poll::Ready(Some(update)) = stream.as_mut().poll_next(&mut context) else {
            panic!("worker result missing");
        };
        assert_eq!(update.generation, 3);
        assert!(matches!(stream.as_mut().poll_next(&mut context), Poll::Ready(None)));
    }

    #[test]
    fn polling_waits_for_a_write_and_uses_the_completed_generation() {
        let shared = Arc::new((Mutex::new((7, true, None, false, 0)), Condvar::new()));
        let worker_state = shared.clone();
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            sender.send(wait_for_poll(&worker_state)).unwrap();
        });
        assert!(matches!(
            receiver.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        {
            let mut state = shared.0.lock().unwrap();
            state.0 = 8;
            state.1 = false;
            shared.1.notify_one();
        }
        assert_eq!(receiver.recv_timeout(Duration::from_secs(2)).unwrap(), Some(8));
        worker.join().unwrap();
    }

    #[test]
    fn shutdown_unblocks_polling_even_during_a_write() {
        let shared = Arc::new((Mutex::new((7, true, None, false, 0)), Condvar::new()));
        let worker_state = shared.clone();
        let (sender, receiver) = mpsc::channel();
        let worker = thread::spawn(move || {
            sender.send(wait_for_poll(&worker_state)).unwrap();
        });
        {
            let mut state = shared.0.lock().unwrap();
            state.3 = true;
            shared.1.notify_one();
        }
        assert_eq!(receiver.recv_timeout(Duration::from_secs(2)).unwrap(), None);
        worker.join().unwrap();
    }
    #[test]
    fn audio_parser_handles_mute_and_rejects_invalid_values() {
        assert_eq!(parse_audio("Volume: 0.42 [MUTED]"), Some((42, true)));
        assert_eq!(parse_audio("Volume: 1.2"), Some((100, false)));
        for value in ["", "Volume: NaN", "Volume: -1", "error"] {
            assert_eq!(parse_audio(value), None);
        }
    }

    #[test]
    fn default_snapshot_does_not_invent_services() {
        let s = Snapshot::default();
        assert!(s.network.is_none() && s.audio.is_none() && s.notifications.is_none());
        assert!(!s.poweroff && !s.reboot && !s.suspend);
    }
}
