//! Private-bus contracts exercise serialized method/property signatures without hardware.
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::Connection;
use zbus::message::Header;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Str};
use zeroize::Zeroizing;

use super::agent::Kind;
use super::{Client, Command as Action, proxy};

const NM: &str = "org.freedesktop.NetworkManager";
const ROOT: &str = "/org/freedesktop/NetworkManager";
const ADAPTER: &str = "/org/bluez/hci0";
const DEVICE: &str = "/org/bluez/hci0/dev_1";
type Settings = HashMap<String, HashMap<String, OwnedValue>>;

fn path(value: &str) -> OwnedObjectPath {
    OwnedObjectPath::try_from(value).unwrap()
}

struct Bus {
    child: Child,
    address: String,
}

impl Bus {
    fn new() -> Self {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut address = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        assert!(
            !address.trim().is_empty(),
            "dbus-daemon did not provide an address; check whether Unix sockets are permitted"
        );
        Self {
            child,
            address: address.trim().to_owned(),
        }
    }

    async fn connect(&self) -> Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .unwrap()
            .method_timeout(Duration::from_secs(5))
            .build()
            .await
            .unwrap()
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Default)]
struct NetworkState {
    enabled: bool,
    scans: usize,
    activated: bool,
    deleted: bool,
    updates: usize,
    received: Option<Settings>,
}

struct Manager(Arc<Mutex<NetworkState>>);

#[zbus::interface(name = "org.freedesktop.NetworkManager")]
impl Manager {
    fn get_devices(&self) -> Vec<OwnedObjectPath> {
        vec![path("/device")]
    }

    #[zbus(property)]
    fn wireless_enabled(&self) -> bool {
        self.0.lock().unwrap().enabled
    }

    #[zbus(property)]
    fn set_wireless_enabled(&self, enabled: bool) {
        self.0.lock().unwrap().enabled = enabled;
    }

    #[zbus(property)]
    fn wireless_hardware_enabled(&self) -> bool {
        true
    }

    fn activate_connection(
        &self,
        connection: OwnedObjectPath,
        device: OwnedObjectPath,
        ap: OwnedObjectPath,
    ) -> OwnedObjectPath {
        assert_eq!(connection.as_str(), "/profile");
        assert_eq!(device.as_str(), "/device");
        assert_eq!(ap.as_str(), "/ap");
        self.0.lock().unwrap().activated = true;
        path("/active")
    }

    fn add_and_activate_connection(
        &self,
        settings: Settings,
        device: OwnedObjectPath,
        ap: OwnedObjectPath,
    ) -> (OwnedObjectPath, OwnedObjectPath) {
        assert_eq!(device.as_str(), "/device");
        assert_eq!(ap.as_str(), "/ap");
        let mut state = self.0.lock().unwrap();
        state.received = Some(settings);
        state.activated = true;
        (path("/profile"), path("/active"))
    }
}

struct Wireless(Arc<Mutex<NetworkState>>);

#[zbus::interface(name = "org.freedesktop.NetworkManager.Device.Wireless")]
impl Wireless {
    fn get_all_access_points(&self) -> Vec<OwnedObjectPath> {
        vec![path("/ap")]
    }

    fn request_scan(&self, options: HashMap<String, OwnedValue>) {
        assert!(options.is_empty());
        self.0.lock().unwrap().scans += 1;
    }

    #[zbus(property)]
    fn active_access_point(&self) -> OwnedObjectPath {
        path(if self.0.lock().unwrap().activated {
            "/ap"
        } else {
            "/"
        })
    }
}

struct NetworkDevice(Arc<Mutex<NetworkState>>);

#[zbus::interface(name = "org.freedesktop.NetworkManager.Device")]
impl NetworkDevice {
    #[zbus(property)]
    fn device_type(&self) -> u32 {
        2
    }

    #[zbus(property)]
    fn interface(&self) -> &str {
        "wlan0"
    }

    #[zbus(property)]
    fn state(&self) -> u32 {
        if self.0.lock().unwrap().activated {
            100
        } else {
            30
        }
    }

    fn disconnect(&self) {
        self.0.lock().unwrap().activated = false;
    }
}

struct AccessPoint;

#[zbus::interface(name = "org.freedesktop.NetworkManager.AccessPoint")]
impl AccessPoint {
    #[zbus(property)]
    fn ssid(&self) -> Vec<u8> {
        b"Test network".to_vec()
    }

    #[zbus(property)]
    fn strength(&self) -> u8 {
        75
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
        0x100
    }
}

struct Profiles(Arc<Mutex<NetworkState>>);

#[zbus::interface(name = "org.freedesktop.NetworkManager.Settings")]
impl Profiles {
    fn list_connections(&self) -> Vec<OwnedObjectPath> {
        if self.0.lock().unwrap().deleted {
            vec![]
        } else {
            vec![path("/profile")]
        }
    }
}

struct Profile(Arc<Mutex<NetworkState>>);

#[zbus::interface(name = "org.freedesktop.NetworkManager.Settings.Connection")]
impl Profile {
    fn get_settings(&self) -> Settings {
        HashMap::from([
            (
                "connection".into(),
                HashMap::from([("id".into(), OwnedValue::from(Str::from("Test network")))]),
            ),
            (
                "802-11-wireless".into(),
                HashMap::from([(
                    "ssid".into(),
                    OwnedValue::try_from(zbus::zvariant::Value::from(b"Test network".as_slice()))
                        .unwrap(),
                )]),
            ),
            (
                "802-11-wireless-security".into(),
                HashMap::from([("key-mgmt".into(), OwnedValue::from(Str::from("wpa-psk")))]),
            ),
        ])
    }

    fn update(&self, settings: Settings) {
        let mut state = self.0.lock().unwrap();
        state.updates += 1;
        state.received = Some(settings);
    }

    fn delete(&self) {
        self.0.lock().unwrap().deleted = true;
    }
}

struct Active;

#[zbus::interface(name = "org.freedesktop.NetworkManager.Connection.Active")]
impl Active {
    #[zbus(property)]
    fn state(&self) -> u32 {
        2
    }
}

async fn network_service(bus: &Bus, state: &Arc<Mutex<NetworkState>>) -> Connection {
    let server = bus.connect().await;
    server.request_name(NM).await.unwrap();
    let objects = server.object_server();
    objects.at(ROOT, Manager(state.clone())).await.unwrap();
    objects
        .at("/device", Wireless(state.clone()))
        .await
        .unwrap();
    objects
        .at("/device", NetworkDevice(state.clone()))
        .await
        .unwrap();
    objects.at("/ap", AccessPoint).await.unwrap();
    objects
        .at(
            "/org/freedesktop/NetworkManager/Settings",
            Profiles(state.clone()),
        )
        .await
        .unwrap();
    objects
        .at("/profile", Profile(state.clone()))
        .await
        .unwrap();
    objects.at("/active", Active).await.unwrap();
    server
}

#[tokio::test]
#[ignore = "requires dbus-daemon; uses a private bus, never the system bus"]
async fn wifi_controls_and_missing_bluetooth_are_independent() {
    let bus = Bus::new();
    let state = Arc::new(Mutex::new(NetworkState::default()));
    let _server = network_service(&bus, &state).await;
    let client = Client::with_connection(bus.connect().await);
    let initial = client.snapshot().await;
    assert!(initial.bluetooth.is_err());
    assert!(!initial.wifi.unwrap().enabled);
    client.execute(Action::WifiPower(true)).await.unwrap();
    client.execute(Action::WifiScan).await.unwrap();
    let wifi = client.snapshot().await.wifi.unwrap();
    assert!(wifi.enabled);
    assert_eq!(wifi.networks[0].name, "Test network");
    assert_eq!(wifi.networks[0].saved.as_deref(), Some("/profile"));
    client
        .execute(Action::WifiConnect(
            wifi.networks[0].clone(),
            Zeroizing::new("updated-password".into()),
        ))
        .await
        .unwrap();
    {
        let state = state.lock().unwrap();
        assert_eq!(state.updates, 1);
        assert_eq!(state.scans, 1);
        assert_eq!(
            <&str>::try_from(&state.received.as_ref().unwrap()["802-11-wireless-security"]["psk"])
                .unwrap(),
            "updated-password"
        );
    }
    assert!(client.snapshot().await.wifi.unwrap().networks[0].connected);
    client
        .execute(Action::WifiDisconnect("/device".into()))
        .await
        .unwrap();
    assert!(!client.snapshot().await.wifi.unwrap().networks[0].connected);
    let mut unsaved = wifi.networks[0].clone();
    unsaved.saved = None;
    client
        .execute(Action::WifiConnect(
            unsaved,
            Zeroizing::new("new-password".into()),
        ))
        .await
        .unwrap();
    client
        .execute(Action::WifiForget("/profile".into()))
        .await
        .unwrap();
    assert!(client.snapshot().await.wifi.unwrap().saved.is_empty());
}

#[derive(Default)]
struct BluetoothState {
    powered: bool,
    discovering: bool,
    paired: bool,
    connected: bool,
    trusted: bool,
    starts: usize,
    stops: usize,
    removed: bool,
    agent: Option<(String, OwnedObjectPath)>,
}

struct Adapter(Arc<Mutex<BluetoothState>>);

#[zbus::interface(name = "org.bluez.Adapter1")]
impl Adapter {
    #[zbus(property)]
    fn alias(&self) -> &str {
        "Laptop"
    }

    #[zbus(property)]
    fn powered(&self) -> bool {
        self.0.lock().unwrap().powered
    }

    #[zbus(property)]
    fn set_powered(&self, powered: bool) {
        self.0.lock().unwrap().powered = powered;
    }

    #[zbus(property)]
    fn discovering(&self) -> bool {
        self.0.lock().unwrap().discovering
    }

    fn start_discovery(&self) {
        let mut s = self.0.lock().unwrap();
        s.discovering = true;
        s.starts += 1;
    }

    fn stop_discovery(&self) {
        let mut s = self.0.lock().unwrap();
        s.discovering = false;
        s.stops += 1;
    }

    fn remove_device(&self, device: OwnedObjectPath) {
        assert_eq!(device.as_str(), DEVICE);
        self.0.lock().unwrap().removed = true;
    }
}

struct BluetoothDevice(Arc<Mutex<BluetoothState>>);

#[zbus::interface(name = "org.bluez.Device1")]
impl BluetoothDevice {
    #[zbus(property)]
    fn adapter(&self) -> OwnedObjectPath {
        path(ADAPTER)
    }

    #[zbus(property)]
    fn alias(&self) -> &str {
        "Headphones"
    }

    #[zbus(property)]
    fn address(&self) -> &str {
        "AA:BB:CC:DD:EE:FF"
    }

    #[zbus(property)]
    fn icon(&self) -> &str {
        "audio-headset"
    }

    #[zbus(property)]
    fn connected(&self) -> bool {
        self.0.lock().unwrap().connected
    }

    #[zbus(property)]
    fn paired(&self) -> bool {
        self.0.lock().unwrap().paired
    }

    #[zbus(property)]
    fn trusted(&self) -> bool {
        self.0.lock().unwrap().trusted
    }

    #[zbus(property)]
    fn set_trusted(&self, trusted: bool) {
        self.0.lock().unwrap().trusted = trusted;
    }

    async fn pair(&self, #[zbus(connection)] connection: &Connection) -> zbus::fdo::Result<()> {
        let (owner, agent) = self.0.lock().unwrap().agent.clone().unwrap();
        proxy(connection, &owner, agent.as_str(), "org.bluez.Agent1")
            .await
            .map_err(zbus::fdo::Error::Failed)?
            .call::<_, _, ()>("RequestConfirmation", &(path(DEVICE), 123456u32))
            .await
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        self.0.lock().unwrap().paired = true;
        Ok(())
    }

    fn connect(&self) {
        self.0.lock().unwrap().connected = true;
    }

    fn disconnect(&self) {
        self.0.lock().unwrap().connected = false;
    }

    fn cancel_pairing(&self) {}
}

struct AgentManager(Arc<Mutex<BluetoothState>>);

#[zbus::interface(name = "org.bluez.AgentManager1")]
impl AgentManager {
    fn register_agent(
        &self,
        agent: OwnedObjectPath,
        capability: String,
        #[zbus(header)] header: Header<'_>,
    ) {
        assert_eq!(capability, "KeyboardDisplay");
        self.0.lock().unwrap().agent = Some((header.sender().unwrap().to_string(), agent));
    }
}

async fn bluetooth_service(bus: &Bus, state: &Arc<Mutex<BluetoothState>>) -> Connection {
    let server = bus.connect().await;
    server.request_name("org.bluez").await.unwrap();
    let objects = server.object_server();
    objects.at("/", zbus::fdo::ObjectManager).await.unwrap();
    objects
        .at("/org/bluez", AgentManager(state.clone()))
        .await
        .unwrap();
    objects.at(ADAPTER, Adapter(state.clone())).await.unwrap();
    objects
        .at(DEVICE, BluetoothDevice(state.clone()))
        .await
        .unwrap();
    server
}

async fn wait_for_prompt(client: &Client) -> super::agent::Prompt {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(prompt) = client.prompt() {
                break prompt;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "requires dbus-daemon; uses a private bus, never the system bus"]
async fn bluetooth_pairing_controls_and_close_cleanup() {
    let bus = Bus::new();
    let state = Arc::new(Mutex::new(BluetoothState::default()));
    let _server = bluetooth_service(&bus, &state).await;
    let client = Client::with_connection(bus.connect().await);
    let epoch = client.epoch();
    client.leave_now();
    assert!(
        client
            .execute_checked(Action::BluetoothPower(ADAPTER.into(), true), epoch)
            .await
            .is_err()
    );
    assert!(!state.lock().unwrap().powered);
    client
        .execute(Action::BluetoothPower(ADAPTER.into(), true))
        .await
        .unwrap();
    client
        .execute(Action::BluetoothScan(ADAPTER.into()))
        .await
        .unwrap();
    client
        .execute(Action::BluetoothScan(ADAPTER.into()))
        .await
        .unwrap();
    assert_eq!(state.lock().unwrap().starts, 1);
    let snapshot = client.snapshot().await;
    assert!(snapshot.wifi.is_err());
    let bluetooth = snapshot.bluetooth.unwrap();
    assert!(bluetooth.adapters[0].powered);
    assert_eq!(bluetooth.devices[0].name, "Headphones");
    let other = client.clone();
    let pairing =
        tokio::spawn(async move { other.execute(Action::BluetoothPair(DEVICE.into())).await });
    let prompt = wait_for_prompt(&client).await;
    assert_eq!(prompt.kind, Kind::Confirm(123456));
    client.reply(prompt.id, Some(Zeroizing::new(String::new())));
    pairing.await.unwrap().unwrap();
    {
        let state = state.lock().unwrap();
        assert!(state.paired && state.connected && state.trusted);
    }
    client
        .execute(Action::BluetoothDisconnect(DEVICE.into()))
        .await
        .unwrap();
    client
        .execute(Action::BluetoothTrust(DEVICE.into(), false))
        .await
        .unwrap();
    assert!(!state.lock().unwrap().trusted);
    client
        .execute(Action::BluetoothConnect(DEVICE.into()))
        .await
        .unwrap();
    client
        .execute(Action::BluetoothForget(ADAPTER.into(), DEVICE.into()))
        .await
        .unwrap();
    assert!(state.lock().unwrap().removed);
    let other = client.clone();
    let pairing =
        tokio::spawn(async move { other.execute(Action::BluetoothPair(DEVICE.into())).await });
    wait_for_prompt(&client).await;
    // Window destruction happens on a UI thread outside Tokio's context.
    let closing = client.clone();
    std::thread::spawn(move || closing.shutdown())
        .join()
        .unwrap();
    assert!(pairing.await.unwrap().is_err());
    assert!(
        client
            .execute(Action::BluetoothConnect(DEVICE.into()))
            .await
            .is_err()
    );
    assert!(client.prompt().is_none());
    tokio::time::timeout(Duration::from_secs(3), async {
        while state.lock().unwrap().discovering {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(state.lock().unwrap().stops, 1);
}

#[tokio::test]
#[ignore = "requires dbus-daemon; uses a private bus, never the system bus"]
async fn bluetooth_agent_recovers_after_service_restart() {
    let bus = Bus::new();
    let state = Arc::new(Mutex::new(BluetoothState::default()));
    let old_server = bluetooth_service(&bus, &state).await;
    let client = Client::with_connection(bus.connect().await);
    let other = client.clone();
    let pairing =
        tokio::spawn(async move { other.execute(Action::BluetoothPair(DEVICE.into())).await });
    let prompt = wait_for_prompt(&client).await;
    client.reply(prompt.id, Some(Zeroizing::new(String::new())));
    pairing.await.unwrap().unwrap();
    old_server.release_name("org.bluez").await.unwrap();
    let new_state = Arc::new(Mutex::new(BluetoothState::default()));
    let _server = bluetooth_service(&bus, &new_state).await;
    let other = client.clone();
    let pairing =
        tokio::spawn(async move { other.execute(Action::BluetoothPair(DEVICE.into())).await });
    let prompt = wait_for_prompt(&client).await;
    client.reply(prompt.id, Some(Zeroizing::new(String::new())));
    pairing.await.unwrap().unwrap();
    assert!(new_state.lock().unwrap().paired);
}
