use std::sync::{Arc, Mutex};

use super::*;

fn network(path: &str, ssid: &[u8], strength: u8, connected: bool) -> WifiNetwork {
    WifiNetwork {
        path: path.into(),
        device: "/device".into(),
        ssid: ssid.to_vec(),
        name: String::from_utf8_lossy(ssid).into_owned(),
        strength,
        connected,
        security: Security::Personal,
        saved: None,
    }
}

#[test]
fn duplicate_access_points_keep_the_active_connection_and_merge_wpa_transition_modes() {
    let mut sae = network("/ap/3", b"Home", 100, false);
    sae.security = Security::Sae;
    let mut open = network("/ap/4", b"Home", 50, false);
    open.security = Security::Open;
    let merged = merge_networks(vec![
        network("/ap/2", b"Home", 90, false),
        network("/ap/1", b"Home", 20, true),
        sae,
        open,
    ]);
    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].path, "/ap/1");
    assert!(merged[0].connected);
    assert_eq!(merged[1].security, Security::Open);
}

#[test]
fn raw_ssids_remain_distinct_even_when_their_display_text_matches() {
    let first = network("/ap/1", b"Home\xff", 50, false);
    let second = network("/ap/2", b"Home\xfe", 50, false);
    assert_eq!(first.name, second.name);
    assert_eq!(merge_networks(vec![first, second]).len(), 2);
}

#[test]
fn password_debug_never_exposes_the_secret() {
    let secret = Password::new("dummy-wifi-password".into());
    assert!(!format!("{secret:?}").contains(secret.as_str()));
    assert_eq!(Password::default().as_str(), "");
}

#[test]
fn signal_changes_do_not_shuffle_different_networks() {
    let first = merge_networks(vec![
        network("/ap/b", b"Beta", 90, false),
        network("/ap/a", b"Alpha", 40, false),
    ]);
    let second = merge_networks(vec![
        network("/ap/b", b"Beta", 40, false),
        network("/ap/a", b"Alpha", 90, false),
    ]);
    assert_eq!(
        first
            .iter()
            .map(|network| &network.ssid)
            .collect::<Vec<_>>(),
        second
            .iter()
            .map(|network| &network.ssid)
            .collect::<Vec<_>>()
    );
}

#[derive(Default)]
struct NetworkState {
    ssid: Vec<u8>,
    saved_ssid: Vec<u8>,
    active_ap: String,
    activated: Vec<(String, String, String)>,
    disconnects: usize,
    written_ssid: Vec<u8>,
    written_password: String,
    creations: usize,
    written_key: String,
    rsn: u32,
    reason: u32,
    failure: Option<(u32, bool)>,
}

struct Manager(Arc<Mutex<NetworkState>>);
#[zbus::interface(name = "org.freedesktop.NetworkManager")]
impl Manager {
    async fn activate_connection(
        &self,
        connection: OwnedObjectPath,
        device: OwnedObjectPath,
        ap: OwnedObjectPath,
        #[zbus(connection)] bus: &zbus::Connection,
    ) -> OwnedObjectPath {
        self.0.lock().unwrap().activated.push((
            connection.to_string(),
            device.to_string(),
            ap.to_string(),
        ));
        let failure = self.0.lock().unwrap().failure;
        if let Some((reason, foreign)) = failure {
            bus.emit_signal(
                None::<&str>,
                "/other",
                "org.freedesktop.NetworkManager.Connection.Active",
                "StateChanged",
                &(4u32, 9u32),
            )
            .await
            .unwrap();
            bus.emit_signal(
                None::<&str>,
                "/active",
                "org.freedesktop.NetworkManager.Connection.Active",
                "StateChanged",
                &(1u32, 1u32),
            )
            .await
            .unwrap();
            {
                let changed = HashMap::from([(
                    "ActiveConnection",
                    Value::from(
                        OwnedObjectPath::try_from(if foreign { "/other" } else { "/" }).unwrap(),
                    ),
                )]);
                bus.emit_signal(
                    None::<&str>,
                    "/device",
                    "org.freedesktop.DBus.Properties",
                    "PropertiesChanged",
                    &(
                        "org.freedesktop.NetworkManager.Device",
                        changed,
                        Vec::<String>::new(),
                    ),
                )
                .await
                .unwrap();
            }
            bus.emit_signal(
                None::<&str>,
                "/device",
                "org.freedesktop.NetworkManager.Device",
                "StateChanged",
                &(120u32, 60u32, reason),
            )
            .await
            .unwrap();
            self.0.lock().unwrap().reason = 0;
            bus.emit_signal(
                None::<&str>,
                "/device",
                "org.freedesktop.NetworkManager.Device",
                "StateChanged",
                &(30u32, 120u32, 0u32),
            )
            .await
            .unwrap();
            bus.emit_signal(
                None::<&str>,
                "/active",
                "org.freedesktop.NetworkManager.Connection.Active",
                "StateChanged",
                &(4u32, 3u32),
            )
            .await
            .unwrap();
        }
        OwnedObjectPath::try_from("/active").unwrap()
    }
    fn add_and_activate_connection(
        &self,
        settings: Settings,
        _device: OwnedObjectPath,
        _ap: OwnedObjectPath,
    ) -> (OwnedObjectPath, OwnedObjectPath) {
        let mut state = self.0.lock().unwrap();
        state.creations += 1;
        state.written_ssid =
            Vec::<u8>::try_from(settings["802-11-wireless"]["ssid"].try_clone().unwrap()).unwrap();
        state.written_password = <&str>::try_from(&settings["802-11-wireless-security"]["psk"])
            .unwrap()
            .to_owned();
        state.written_key = <&str>::try_from(&settings["802-11-wireless-security"]["key-mgmt"])
            .unwrap()
            .to_owned();
        (
            OwnedObjectPath::try_from("/saved").unwrap(),
            OwnedObjectPath::try_from("/active").unwrap(),
        )
    }
}

struct AccessPoint(Arc<Mutex<NetworkState>>);
#[zbus::interface(name = "org.freedesktop.NetworkManager.AccessPoint")]
impl AccessPoint {
    #[zbus(property)]
    fn mode(&self) -> u32 {
        2
    }

    #[zbus(property)]
    fn ssid(&self) -> Vec<u8> {
        self.0.lock().unwrap().ssid.clone()
    }
    #[zbus(property)]
    fn flags(&self) -> u32 {
        1
    }
    #[zbus(property)]
    fn wpa_flags(&self) -> u32 {
        0
    }
    #[zbus(property)]
    fn rsn_flags(&self) -> u32 {
        self.0.lock().unwrap().rsn
    }
}

struct Wireless(Arc<Mutex<NetworkState>>);
#[zbus::interface(name = "org.freedesktop.NetworkManager.Device.Wireless")]
impl Wireless {
    #[zbus(property)]
    fn active_access_point(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from(self.0.lock().unwrap().active_ap.as_str()).unwrap()
    }
}
struct Device(Arc<Mutex<NetworkState>>);
#[zbus::interface(name = "org.freedesktop.NetworkManager.Device")]
impl Device {
    #[zbus(property)]
    fn state_reason(&self) -> (u32, u32) {
        (30, self.0.lock().unwrap().reason)
    }
    #[zbus(property)]
    fn available_connections(&self) -> Vec<OwnedObjectPath> {
        vec![OwnedObjectPath::try_from("/saved").unwrap()]
    }

    fn disconnect(&self) {
        self.0.lock().unwrap().disconnects += 1;
    }
}
struct Saved(Arc<Mutex<NetworkState>>);
#[zbus::interface(name = "org.freedesktop.NetworkManager.Settings.Connection")]
impl Saved {
    fn get_settings(&self) -> Settings {
        let state = self.0.lock().unwrap();
        HashMap::from([
            (
                "802-11-wireless".into(),
                HashMap::from([(
                    "ssid".into(),
                    Value::from(state.saved_ssid.as_slice())
                        .try_to_owned()
                        .unwrap(),
                )]),
            ),
            (
                "802-11-wireless-security".into(),
                HashMap::from([(
                    "key-mgmt".into(),
                    Value::from("wpa-psk").try_to_owned().unwrap(),
                )]),
            ),
        ])
    }
    fn update(&self, settings: Settings) {
        let mut state = self.0.lock().unwrap();
        state.written_password = <&str>::try_from(&settings["802-11-wireless-security"]["psk"])
            .unwrap()
            .to_owned();
    }
}
struct SettingsManager;
#[zbus::interface(name = "org.freedesktop.NetworkManager.Settings")]
impl SettingsManager {
    fn list_connections(&self) -> Vec<OwnedObjectPath> {
        vec![OwnedObjectPath::try_from("/saved").unwrap()]
    }
}
struct Active(Arc<Mutex<NetworkState>>);
#[zbus::interface(name = "org.freedesktop.NetworkManager.Connection.Active")]
impl Active {
    #[zbus(property)]
    fn state(&self) -> u32 {
        if self.0.lock().unwrap().failure.is_some() {
            4
        } else {
            2
        }
    }
}

#[test]
#[ignore = "requires dbus-daemon; uses a private test bus"]
fn private_bus_wifi_activation_checks_the_selected_target_and_keeps_credentials_off_argv() {
    let bus = crate::status::tests::TestBus::new();
    let state = Arc::new(Mutex::new(NetworkState {
        ssid: b"Home".to_vec(),
        saved_ssid: b"Home".to_vec(),
        active_ap: "/ap/other".into(),
        rsn: 0x100,
        ..Default::default()
    }));
    let _server = zbus::blocking::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name(NM)
        .unwrap()
        .serve_at(NM_PATH, Manager(state.clone()))
        .unwrap()
        .serve_at("/ap/selected", AccessPoint(state.clone()))
        .unwrap()
        .serve_at("/device", Wireless(state.clone()))
        .unwrap()
        .serve_at("/device", Device(state.clone()))
        .unwrap()
        .serve_at("/saved", Saved(state.clone()))
        .unwrap()
        .serve_at("/org/freedesktop/NetworkManager/Settings", SettingsManager)
        .unwrap()
        .serve_at("/active", Active(state.clone()))
        .unwrap()
        .build()
        .unwrap();
    let client = bus.connect();
    let mut selected = network("/ap/selected", b"Home", 70, true);
    let empty = Password::default();
    assert!(connect_wifi_on(&client, &selected, &empty).is_err());
    assert_eq!(
        state.lock().unwrap().disconnects,
        0,
        "a stale row must not disconnect another network"
    );
    state.lock().unwrap().active_ap = selected.path.clone();
    connect_wifi_on(&client, &selected, &empty).unwrap();
    assert_eq!(state.lock().unwrap().disconnects, 1);
    selected.connected = false;
    selected.saved = Some("/saved".into());
    state.lock().unwrap().saved_ssid = b"Other".to_vec();
    assert!(connect_wifi_on(&client, &selected, &empty).is_err());
    assert!(state.lock().unwrap().activated.is_empty());
    state.lock().unwrap().saved_ssid = b"Home".to_vec();
    connect_wifi_on(&client, &selected, &empty).unwrap();
    assert_eq!(
        state.lock().unwrap().activated,
        [("/saved".into(), "/device".into(), "/ap/selected".into())]
    );
    state.lock().unwrap().ssid = b"Changed".to_vec();
    assert!(connect_wifi_on(&client, &selected, &empty).is_err());
    assert_eq!(state.lock().unwrap().activated.len(), 1);
    state.lock().unwrap().ssid = b"Home".to_vec();
    selected.saved = None;
    connect_wifi_on(
        &client,
        &selected,
        &Password::new("corrected-password".into()),
    )
    .unwrap();
    assert_eq!(state.lock().unwrap().written_password, "corrected-password");
    assert_eq!(state.lock().unwrap().activated.len(), 2);
    assert_eq!(
        state.lock().unwrap().creations,
        0,
        "retry must reuse the profile that was created before authentication failed"
    );
    selected.saved = None;
    selected.ssid = b"Raw\xff".to_vec();
    state.lock().unwrap().ssid = selected.ssid.clone();
    connect_wifi_on(&client, &selected, &Password::new("dummy-password".into())).unwrap();
    assert_eq!(state.lock().unwrap().written_ssid, b"Raw\xff");
    assert_eq!(state.lock().unwrap().written_password, "dummy-password");
    assert_eq!(state.lock().unwrap().creations, 1);
    // A transition profile permits WPA2 fallback; NetworkManager selects the
    // actual authentication method for the adapter's capabilities.
    state.lock().unwrap().rsn = 0x500;
    selected.security = Security::Transition;
    connect_wifi_on(&client, &selected, &Password::new("dummy-password".into())).unwrap();
    assert_eq!(state.lock().unwrap().written_key, "wpa-psk");
    state.lock().unwrap().rsn = 0x400;
    selected.security = Security::Sae;
    connect_wifi_on(&client, &selected, &Password::new("dummy-password".into())).unwrap();
    assert_eq!(state.lock().unwrap().written_key, "sae");
    selected.ssid = b"Home".to_vec();
    selected.security = Security::Personal;
    selected.saved = Some("/saved".into());
    state.lock().unwrap().ssid = selected.ssid.clone();
    state.lock().unwrap().rsn = 0x100;
    for (reason, foreign, retry) in [(7, false, true), (17, false, false), (7, true, false)] {
        state.lock().unwrap().failure = Some((reason, foreign));
        let error = connect_wifi_on(&client, &selected, &Password::default()).unwrap_err();
        assert_eq!(
            state.lock().unwrap().reason,
            0,
            "the device reason was reset before activation returned"
        );
        assert_eq!(
            error.retry_password, retry,
            "reason {reason}, foreign={foreign}"
        );
    }
}

struct Adapter(Arc<Mutex<std::collections::HashSet<String>>>);
#[zbus::interface(name = "org.bluez.Adapter1")]
impl Adapter {
    fn start_discovery(&self, #[zbus(header)] header: zbus::message::Header<'_>) {
        self.0
            .lock()
            .unwrap()
            .insert(header.sender().unwrap().to_string());
    }
    fn stop_discovery(&self, #[zbus(header)] header: zbus::message::Header<'_>) {
        self.0
            .lock()
            .unwrap()
            .remove(header.sender().unwrap().as_str());
    }
}

#[test]
#[ignore = "requires dbus-daemon; uses a private test bus"]
fn private_bus_closing_discovery_releases_only_this_shells_session() {
    let bus = crate::status::tests::TestBus::new();
    let sessions = Arc::new(Mutex::new(std::collections::HashSet::new()));
    let _server = zbus::blocking::connection::Builder::address(bus.address.as_str())
        .unwrap()
        .name("org.bluez")
        .unwrap()
        .serve_at("/adapter", Adapter(sessions.clone()))
        .unwrap()
        .build()
        .unwrap();
    let other = bus.connect();
    other
        .call_method(
            Some("org.bluez"),
            "/adapter",
            Some("org.bluez.Adapter1"),
            "StartDiscovery",
            &(),
        )
        .unwrap();
    let mut discovery = DiscoverySession::default();
    discovery
        .start_bluetooth(bus.connect(), "/adapter".into())
        .unwrap();
    assert_eq!(sessions.lock().unwrap().len(), 2);
    discovery.set(crate::status::Discovery::None).unwrap();
    assert_eq!(
        sessions.lock().unwrap().iter().cloned().collect::<Vec<_>>(),
        [other.unique_name().unwrap().to_string()]
    );
    drop(discovery);
    assert_eq!(sessions.lock().unwrap().len(), 1);
}

struct FlakyAdapter {
    attempts: Arc<std::sync::atomic::AtomicU32>,
    stops: Arc<std::sync::atomic::AtomicU32>,
}

#[zbus::interface(name = "org.bluez.Adapter1")]
impl FlakyAdapter {
    fn start_discovery(&self) -> zbus::fdo::Result<()> {
        if self
            .attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            == 0
        {
            Err(zbus::fdo::Error::Failed("temporarily unavailable".into()))
        } else {
            Ok(())
        }
    }
    fn stop_discovery(&self) {
        self.stops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[test]
fn private_bus_discovery_worker_recovers_without_another_request_and_stops_retries_on_close() {
    use crate::status::{Action, Discovery};
    use std::sync::{
        Condvar,
        atomic::{AtomicU8, AtomicU32, Ordering},
    };
    use std::time::{Duration, Instant};
    for close_before_retry in [false, true] {
        let bus = crate::status::tests::TestBus::new();
        let attempts = Arc::new(AtomicU32::new(0));
        let stops = Arc::new(AtomicU32::new(0));
        let _server = zbus::blocking::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name("org.bluez")
            .unwrap()
            .serve_at(
                "/adapter",
                FlakyAdapter {
                    attempts: attempts.clone(),
                    stops: stops.clone(),
                },
            )
            .unwrap()
            .build()
            .unwrap();
        let private = bus.connect();
        let (commands, receiver) = std::sync::mpsc::sync_channel(8);
        let state = Arc::new((Mutex::new((0, false, None, false, 0)), Condvar::new()));
        let worker_state = state.clone();
        let worker = std::thread::spawn(move || {
            let mut discovery = DiscoverySession::default();
            crate::status::command_worker(
                receiver,
                Arc::new(Mutex::new(None)),
                worker_state,
                Arc::new(AtomicU8::new(0)),
                |kind| match kind {
                    Discovery::Bluetooth => discovery
                        .start_bluetooth(private.clone(), "/adapter".into())
                        .map_err(|error| error.to_string()),
                    Discovery::None => {
                        discovery.stop();
                        Ok(())
                    }
                    Discovery::Wifi => unreachable!(),
                },
            );
        });
        type CommandState = (u64, bool, Option<crate::status::ActionError>, bool, u64);
        let wait = |condition: &dyn Fn(&CommandState) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(4);
            let mut current = state.0.lock().unwrap();
            while !condition(&current) {
                let (next, timeout) = state
                    .1
                    .wait_timeout(current, deadline.saturating_duration_since(Instant::now()))
                    .unwrap();
                current = next;
                assert!(
                    !timeout.timed_out(),
                    "discovery worker did not progress: {current:?}"
                );
            }
        };
        commands
            .send((1, Action::Discover(Discovery::Bluetooth)))
            .unwrap();
        wait(&|state| !state.1 && state.2.is_some());
        if !close_before_retry {
            wait(&|state| attempts.load(Ordering::SeqCst) == 2 && !state.1 && state.2.is_none());
        }
        commands
            .send((2, Action::Discover(Discovery::None)))
            .unwrap();
        wait(&|state| state.0 == 2 && !state.1);
        drop(commands);
        worker.join().unwrap();
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            if close_before_retry { 1 } else { 2 }
        );
        assert_eq!(
            stops.load(Ordering::SeqCst),
            if close_before_retry { 0 } else { 1 }
        );
    }
}

#[path = "edge_cases.rs"]
mod edge_cases;
