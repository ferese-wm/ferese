//! Discovery belongs to the open menu; connection work runs outside status polling.
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::time::Duration;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zeroize::Zeroizing;
mod activation;
pub(crate) mod pairing;
mod profiles;
use profiles::{SavedProfile, WifiSecurity};
#[cfg(test)]
mod tests;

const NM: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const AP: &str = "org.freedesktop.NetworkManager.AccessPoint";
const SETTINGS: &str = "org.freedesktop.NetworkManager.Settings.Connection";
type Settings = HashMap<String, HashMap<String, OwnedValue>>;

#[derive(Clone, Default)]
pub(crate) struct Password(Zeroizing<String>);

impl Password {
    pub fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password([redacted])")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Security {
    Open,
    Personal,
    Transition,
    Sae,
    Enterprise,
    Owe,
    Unsupported,
}

impl Security {
    pub fn needs_password(self) -> bool {
        matches!(self, Self::Personal | Self::Transition | Self::Sae)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WifiNetwork {
    pub path: String,
    pub device: String,
    pub ssid: Vec<u8>,
    pub name: String,
    pub strength: u8,
    pub connected: bool,
    pub security: Security,
    pub saved: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BluetoothDevice {
    pub path: String,
    pub name: String,
    pub paired: bool,
    pub connected: bool,
}

fn properties(
    connection: &Connection,
    service: &str,
    path: &str,
    interface: &str,
) -> zbus::Result<HashMap<String, OwnedValue>> {
    connection
        .call_method(
            Some(service),
            path,
            Some("org.freedesktop.DBus.Properties"),
            "GetAll",
            &(interface,),
        )?
        .body()
        .deserialize()
}

fn security(flags: u32, wpa: u32, rsn: u32) -> Security {
    WifiSecurity { flags, wpa, rsn }.kind()
}

fn saved_networks(connection: &Connection, device: &str) -> zbus::Result<Vec<SavedProfile>> {
    let paths: Vec<OwnedObjectPath> = super::network::property(
        connection,
        device,
        "org.freedesktop.NetworkManager.Device",
        "AvailableConnections",
    )?;
    let mut profiles = Vec::new();
    for path in paths {
        let message =
            connection.call_method(Some(NM), path.as_str(), Some(SETTINGS), "GetSettings", &())?;
        profiles.push(SavedProfile {
            path: path.to_string(),
            settings: message.body().deserialize()?,
        });
    }
    profiles.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(profiles)
}

pub(super) fn wifi_networks(
    connection: &Connection,
    devices: &[OwnedObjectPath],
) -> zbus::Result<Vec<WifiNetwork>> {
    let mut networks = Vec::new();
    for device in devices {
        let profiles = saved_networks(connection, device.as_str())?;
        let wireless = properties(connection, NM, device.as_str(), WIRELESS)?;
        let active = wireless
            .get("ActiveAccessPoint")
            .and_then(|value| <&zbus::zvariant::ObjectPath>::try_from(value).ok());
        let activated = super::network::property::<u32>(
            connection,
            device.as_str(),
            "org.freedesktop.NetworkManager.Device",
            "State",
        )
        .ok()
            == Some(100);
        let paths: Vec<OwnedObjectPath> = connection
            .call_method(
                Some(NM),
                device.as_str(),
                Some(WIRELESS),
                "GetAllAccessPoints",
                &(),
            )?
            .body()
            .deserialize()?;
        for path in paths {
            let Ok(ap) = properties(connection, NM, path.as_str(), AP) else {
                continue;
            };
            let Some(ssid) = ap
                .get("Ssid")
                .and_then(|value| value.try_clone().ok())
                .and_then(|value| Vec::<u8>::try_from(value).ok())
                .filter(|ssid| !ssid.is_empty() && ssid.len() <= 32)
            else {
                continue;
            };
            let number = |name: &str| {
                ap.get(name)
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or_default()
            };
            let security = security(number("Flags"), number("WpaFlags"), number("RsnFlags"));
            let saved = profiles
                .iter()
                .find(|profile| profile.matches(&ssid, &ap))
                .map(|profile| profile.path.clone());
            networks.push(WifiNetwork {
                name: String::from_utf8_lossy(&ssid)
                    .chars()
                    .map(|c| if c.is_control() { '�' } else { c })
                    .collect(),
                ssid,
                path: path.to_string(),
                device: device.to_string(),
                strength: ap
                    .get("Strength")
                    .and_then(|value| u8::try_from(value).ok())
                    .unwrap_or_default()
                    .min(100),
                connected: activated
                    && active.is_some_and(|active| active.as_str() == path.as_str()),
                security,
                saved,
            });
        }
    }
    Ok(merge_networks(networks))
}

fn merge_networks(networks: Vec<WifiNetwork>) -> Vec<WifiNetwork> {
    let mut unique = BTreeMap::new();
    for network in networks {
        let family = if matches!(network.security, Security::Sae | Security::Transition) {
            Security::Personal
        } else {
            network.security
        };
        let key = (network.ssid.clone(), family);
        let replace = unique.get(&key).is_none_or(|old: &WifiNetwork| {
            (
                network.connected,
                network.saved.is_some(),
                network.strength,
                &network.path,
            ) > (old.connected, old.saved.is_some(), old.strength, &old.path)
        });
        if replace {
            unique.insert(key, network);
        }
    }
    let mut networks: Vec<_> = unique.into_values().collect();
    networks.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then_with(|| b.saved.is_some().cmp(&a.saved.is_some()))
            .then_with(|| a.name.cmp(&b.name))
    });
    networks
}

pub(super) fn bluetooth_devices(
    objects: &zbus::fdo::ManagedObjects,
    adapter: &str,
) -> Vec<BluetoothDevice> {
    let mut devices = objects
        .iter()
        .filter_map(|(path, interfaces)| {
            let properties = interfaces.get("org.bluez.Device1")?;
            let owner = properties
                .get("Adapter")
                .and_then(|value| <&zbus::zvariant::ObjectPath>::try_from(value).ok())?;
            if owner.as_str() != adapter {
                return None;
            }
            let name = ["Alias", "Name", "Address"].into_iter().find_map(|name| {
                properties
                    .get(name)
                    .and_then(|value| <&str>::try_from(value).ok())
            })?;
            let flag = |name: &str| {
                properties
                    .get(name)
                    .and_then(|value| bool::try_from(value).ok())
                    .unwrap_or_default()
            };
            Some(BluetoothDevice {
                path: path.to_string(),
                name: name.to_owned(),
                paired: flag("Paired"),
                connected: flag("Connected"),
            })
        })
        .collect::<Vec<_>>();
    devices.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then_with(|| b.paired.cmp(&a.paired))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.path.cmp(&b.path))
    });
    devices
}

#[derive(Default)]
pub(super) struct DiscoverySession {
    bluetooth: Option<(Connection, String)>,
}

impl DiscoverySession {
    pub(super) fn set(&mut self, discovery: super::Discovery) -> Result<(), String> {
        self.stop();
        if discovery == super::Discovery::None {
            return Ok(());
        }
        let connection = zbus::blocking::connection::Builder::system()
            .and_then(|builder| builder.method_timeout(Duration::from_secs(2)).build())
            .map_err(|error| error.to_string())?;
        match discovery {
            super::Discovery::None => {}
            super::Discovery::Wifi => {
                let devices: Vec<OwnedObjectPath> =
                    super::network::property(&connection, NM_PATH, NM, "Devices")
                        .map_err(|error| error.to_string())?;
                for device in devices {
                    if super::network::property::<u32>(
                        &connection,
                        device.as_str(),
                        "org.freedesktop.NetworkManager.Device",
                        "DeviceType",
                    )
                    .ok()
                        == Some(2)
                    {
                        // NetworkManager may rate-limit a new request; its cached results remain useful.
                        let _ = connection.call_method(
                            Some(NM),
                            device.as_str(),
                            Some(WIRELESS),
                            "RequestScan",
                            &(HashMap::<&str, Value<'_>>::new(),),
                        );
                    }
                }
            }
            super::Discovery::Bluetooth => {
                let adapter = super::bluetooth::adapter(&connection)
                    .map_err(|error| error.to_string())?
                    .ok_or("No Bluetooth adapter available")?;
                self.start_bluetooth(connection, adapter)
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }

    fn start_bluetooth(&mut self, connection: Connection, adapter: String) -> zbus::Result<()> {
        connection.call_method(
            Some("org.bluez"),
            adapter.as_str(),
            Some("org.bluez.Adapter1"),
            "StartDiscovery",
            &(),
        )?;
        self.bluetooth = Some((connection, adapter));
        Ok(())
    }

    fn stop(&mut self) {
        if let Some((connection, adapter)) = self.bluetooth.take() {
            let _ = connection.call_method(
                Some("org.bluez"),
                adapter.as_str(),
                Some("org.bluez.Adapter1"),
                "StopDiscovery",
                &(),
            );
        }
    }
}

impl Drop for DiscoverySession {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Clone, Debug)]
pub(crate) struct WifiError {
    message: String,
    pub retry_password: bool,
}

impl fmt::Display for WifiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl From<String> for WifiError {
    fn from(message: String) -> Self {
        Self {
            message,
            retry_password: false,
        }
    }
}

impl From<&str> for WifiError {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

pub(crate) fn connect_wifi(network: &WifiNetwork, password: &Password) -> Result<(), WifiError> {
    let connection = zbus::blocking::connection::Builder::system()
        .and_then(|builder| builder.method_timeout(Duration::from_secs(30)).build())
        .map_err(|error| error.to_string())?;
    connect_wifi_on(&connection, network, password)
}

fn connect_wifi_on(
    connection: &Connection,
    network: &WifiNetwork,
    password: &Password,
) -> Result<(), WifiError> {
    let authority: String = connection
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetNameOwner",
            &(NM,),
        )
        .map_err(|error| error.to_string())?
        .body()
        .deserialize()
        .map_err(|error| error.to_string())?;
    let current = properties(connection, &authority, &network.path, AP)
        .map_err(|_| "This network is no longer available")?;
    let ssid: Vec<u8> = current
        .get("Ssid")
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| Vec::<u8>::try_from(value).ok())
        .ok_or("This network is no longer available")?;
    if ssid != network.ssid {
        return Err("This network changed. Refresh and select it again.".into());
    }
    let number = |name: &str| {
        current
            .get(name)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or_default()
    };
    if security(number("Flags"), number("WpaFlags"), number("RsnFlags")) != network.security {
        return Err("The network security changed. Refresh and select it again.".into());
    }
    let device =
        OwnedObjectPath::try_from(network.device.as_str()).map_err(|error| error.to_string())?;
    let ap = OwnedObjectPath::try_from(network.path.as_str()).map_err(|error| error.to_string())?;
    if network.connected {
        let active: OwnedObjectPath =
            super::network::property(connection, device.as_str(), WIRELESS, "ActiveAccessPoint")
                .map_err(|error| error.to_string())?;
        if active != ap {
            return Err("The active network changed. Refresh and select it again.".into());
        }
        connection
            .call_method(
                Some(authority.as_str()),
                device.as_str(),
                Some("org.freedesktop.NetworkManager.Device"),
                "Disconnect",
                &(),
            )
            .map_err(|error| error.to_string())?;
        return Ok(());
    }
    let profiles = saved_networks(connection, &network.device)
        .map_err(|_| "Could not check saved connections. Try refreshing.".to_owned())?;
    // Recheck every restriction against this AP, even when the view had a saved target.
    let saved = profiles
        .iter()
        .position(|profile| {
            network.saved.as_deref() == Some(profile.path.as_str())
                && profile.matches(&ssid, &current)
        })
        .or_else(|| {
            profiles
                .iter()
                .position(|profile| profile.matches(&ssid, &current))
        });
    if network.saved.is_some() && saved.is_none() {
        return Err(
            "The saved connection is no longer compatible. Refresh and select the network again."
                .into(),
        );
    }
    // Subscribe before activation: failure can precede the method's reply.
    let mut events = activation::Events::subscribe(connection, &network.device, &authority)?;
    let active = if let Some(index) = saved {
        let profile = profiles
            .into_iter()
            .nth(index)
            .expect("selected profile belongs to this list");
        let saved =
            OwnedObjectPath::try_from(profile.path.as_str()).map_err(|error| error.to_string())?;
        let stored = profile.settings;
        if network.security.needs_password() && !password.as_str().is_empty() {
            let mut update: HashMap<String, HashMap<String, Value<'_>>> = stored
                .into_iter()
                .map(|(section, values)| {
                    (
                        section,
                        values
                            .into_iter()
                            .map(|(key, value)| (key, value.into()))
                            .collect(),
                    )
                })
                .collect();
            update
                .entry("802-11-wireless-security".into())
                .or_default()
                .insert("psk".into(), Value::from(password.as_str()));
            connection
                .call_method(
                    Some(authority.as_str()),
                    saved.as_str(),
                    Some(SETTINGS),
                    "Update",
                    &(update,),
                )
                .map_err(|error| error.to_string())?;
        }
        connection
            .call_method(
                Some(authority.as_str()),
                NM_PATH,
                Some(NM),
                "ActivateConnection",
                &(saved, device, ap),
            )
            .map_err(|error| error.to_string())?
            .body()
            .deserialize::<OwnedObjectPath>()
            .map_err(|error| error.to_string())?
    } else {
        let mut settings = HashMap::<&str, HashMap<&str, Value<'_>>>::new();
        settings.insert(
            "connection",
            [
                ("id", Value::from(network.name.as_str())),
                ("type", Value::from("802-11-wireless")),
            ]
            .into(),
        );
        settings.insert(
            "802-11-wireless",
            [("ssid", Value::from(network.ssid.as_slice()))].into(),
        );
        let key = match network.security {
            Security::Open => None,
            Security::Personal | Security::Transition => Some("wpa-psk"),
            Security::Sae => Some("sae"),
            Security::Owe => Some("owe"),
            _ => return Err("Set up this network in Network settings first.".into()),
        };
        if let Some(key) = key {
            let mut security = HashMap::from([("key-mgmt", Value::from(key))]);
            if network.security.needs_password() {
                security.insert("psk", Value::from(password.as_str()));
            }
            settings.insert("802-11-wireless-security", security);
        }
        let (_, active): (OwnedObjectPath, OwnedObjectPath) = connection
            .call_method(
                Some(authority.as_str()),
                NM_PATH,
                Some(NM),
                "AddAndActivateConnection",
                &(settings, device, ap),
            )
            .map_err(|error| error.to_string())?
            .body()
            .deserialize()
            .map_err(|error| error.to_string())?;
        active
    };
    let proxy = Proxy::new(
        connection,
        authority.as_str(),
        active.as_str(),
        "org.freedesktop.NetworkManager.Connection.Active",
    )
    .map_err(|error| error.to_string())?;
    let deadline = std::time::Instant::now() + Duration::from_secs(45);
    loop {
        let state = proxy.get_property::<u32>("State");
        events.drain(active.as_str())?;
        match state {
            Ok(2) => return Ok(()),
            Ok(3 | 4) | Err(_) => {
                return Err(events.failure());
            }
            _ if std::time::Instant::now() >= deadline => {
                return Err("The connection timed out. Check the network and try again.".into());
            }
            _ => std::thread::sleep(Duration::from_millis(250)),
        }
    }
}

fn connect_bluetooth(device: &BluetoothDevice, agent: pairing::Agent) -> Result<(), String> {
    let authority = agent.authority.clone();
    let cancelled = agent.cancelled.clone();
    let phase = agent.phase.clone();
    let check_cancelled = || {
        if cancelled.load(std::sync::atomic::Ordering::Acquire) {
            Err("Pairing canceled".to_owned())
        } else {
            Ok(())
        }
    };
    check_cancelled()?;
    let connection = zbus::blocking::connection::Builder::system()
        .and_then(|builder| {
            builder
                .method_timeout(Duration::from_secs(60))
                .serve_at(pairing::PATH, agent)?
                .build()
        })
        .map_err(|error| error.to_string())?;
    connect_bluetooth_on(connection, device, authority, cancelled, phase)
}

fn connect_bluetooth_on(
    connection: Connection,
    device: &BluetoothDevice,
    authority: std::sync::Arc<std::sync::OnceLock<String>>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    phase: std::sync::Arc<std::sync::atomic::AtomicU8>,
) -> Result<(), String> {
    let check_cancelled = || {
        if cancelled.load(std::sync::atomic::Ordering::Acquire) {
            Err("Pairing canceled".to_owned())
        } else {
            Ok(())
        }
    };
    let owner: String = connection
        .call_method(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            Some("org.freedesktop.DBus"),
            "GetNameOwner",
            &("org.bluez",),
        )
        .map_err(|error| error.to_string())?
        .body()
        .deserialize()
        .map_err(|error| error.to_string())?;
    authority
        .set(owner)
        .map_err(|_| "The Bluetooth authority changed".to_owned())?;
    let proxy = Proxy::new(
        &connection,
        "org.bluez",
        device.path.as_str(),
        "org.bluez.Device1",
    )
    .map_err(|error| error.to_string())?;
    if device.connected {
        if proxy
            .get_property::<bool>("Connected")
            .map_err(|error| error.to_string())?
        {
            proxy
                .call::<_, _, ()>("Disconnect", &())
                .map_err(|error| error.to_string())?;
        }
    } else {
        if proxy
            .get_property::<bool>("Connected")
            .map_err(|error| error.to_string())?
        {
            return Ok(());
        }
        connection
            .call_method(
                Some("org.bluez"),
                "/org/bluez",
                Some("org.bluez.AgentManager1"),
                "RegisterAgent",
                &(
                    OwnedObjectPath::try_from(pairing::PATH).expect("static agent path is valid"),
                    "KeyboardDisplay",
                ),
            )
            .map_err(|error| error.to_string())?;
        let paired: bool = proxy
            .get_property("Paired")
            .map_err(|error| error.to_string())?;
        let was_trusted = paired
            || proxy
                .get_property::<bool>("Trusted")
                .map_err(|error| error.to_string())?;
        if !paired {
            phase.store(
                pairing::Phase::Pairing as u8,
                std::sync::atomic::Ordering::Release,
            );
            check_cancelled()?;
            proxy
                .call::<_, _, ()>("Pair", &())
                .map_err(|error| error.to_string())?;
        }
        check_cancelled()?;
        phase.store(
            pairing::Phase::Connecting as u8,
            std::sync::atomic::Ordering::Release,
        );
        check_cancelled()?;
        proxy
            .call::<_, _, ()>("Connect", &())
            .map_err(|error| error.to_string())?;
        check_cancelled()?;
        phase.store(
            pairing::Phase::Connected as u8,
            std::sync::atomic::Ordering::Release,
        );
        if !paired && !was_trusted {
            proxy.set_property("Trusted", true).map_err(|_| {
                "The device connected, but automatic reconnection could not be enabled.".to_owned()
            })?;
            phase.store(
                pairing::Phase::TrustEnabled as u8,
                std::sync::atomic::Ordering::Release,
            );
        }
    }
    Ok(())
}

pub(crate) fn open_settings(radio: super::Discovery) -> Result<(), String> {
    let program = if radio == super::Discovery::Wifi {
        "nm-connection-editor"
    } else {
        "blueman-manager"
    };
    super::execute(&super::Action::Settings, Some(&[program.to_owned()]))
}
