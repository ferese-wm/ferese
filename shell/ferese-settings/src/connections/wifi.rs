use std::collections::{BTreeMap, HashMap};

use zbus::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use super::proxy;

const NM: &str = "org.freedesktop.NetworkManager";
const ROOT: &str = "/org/freedesktop/NetworkManager";
const DEVICE: &str = "org.freedesktop.NetworkManager.Device";
const WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const AP: &str = "org.freedesktop.NetworkManager.AccessPoint";
const SETTINGS: &str = "org.freedesktop.NetworkManager.Settings";
const PROFILE: &str = "org.freedesktop.NetworkManager.Settings.Connection";
type Settings = HashMap<String, HashMap<String, OwnedValue>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Security {
    Open,
    Personal,
    Sae,
    Owe,
    Enterprise,
    Unsupported,
}

impl Security {
    pub fn label(self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Personal => "WPA / WPA2",
            Self::Sae => "WPA3",
            Self::Owe => "Enhanced open",
            Self::Enterprise => "Enterprise",
            Self::Unsupported => "Legacy security",
        }
    }

    pub fn password(self) -> bool {
        matches!(self, Self::Personal | Self::Sae)
    }

    pub fn can_create(self) -> bool {
        !matches!(self, Self::Enterprise | Self::Unsupported)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Network {
    pub ssid: Vec<u8>,
    pub name: String,
    pub security: Security,
    pub strength: u8,
    pub device: String,
    pub ap: String,
    pub saved: Option<String>,
    pub connected: bool,
    pub connecting: bool,
    pub hidden: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Radio {
    pub path: String,
    pub name: String,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Wifi {
    pub enabled: bool,
    pub hardware_enabled: bool,
    pub radios: Vec<Radio>,
    pub networks: Vec<Network>,
    pub saved: Vec<Saved>,
}

#[derive(Clone, Debug)]
pub(crate) struct Saved {
    pub path: String,
    pub name: String,
    pub ssid: Vec<u8>,
    pub security: Security,
}

fn security(flags: u32, wpa: u32, rsn: u32) -> Security {
    let auth = wpa | rsn;
    if auth & (0x200 | 0x2000) != 0 {
        Security::Enterprise
    } else if auth & 0x100 != 0 {
        Security::Personal
    } else if auth & 0x400 != 0 {
        Security::Sae
    } else if auth & (0x800 | 0x1000) != 0 {
        Security::Owe
    } else if flags & 1 != 0 {
        Security::Unsupported
    } else {
        Security::Open
    }
}

pub(crate) async fn read(connection: &Connection) -> Result<Wifi, String> {
    let root = proxy(connection, NM, ROOT, NM).await?;
    let enabled = root
        .get_property("WirelessEnabled")
        .await
        .map_err(|e| e.to_string())?;
    let hardware_enabled = root
        .get_property("WirelessHardwareEnabled")
        .await
        .map_err(|e| e.to_string())?;
    let devices: Vec<OwnedObjectPath> = root
        .call("GetDevices", &())
        .await
        .map_err(|e| e.to_string())?;
    let saved = profiles(connection).await?;
    let mut result = Wifi {
        enabled,
        hardware_enabled,
        saved,
        ..Wifi::default()
    };
    let mut networks = BTreeMap::new();
    for path in devices {
        let device = proxy(connection, NM, path.as_str(), DEVICE).await?;
        if device.get_property::<u32>("DeviceType").await.unwrap_or(0) != 2 {
            continue;
        }
        let name: String = device
            .get_property("Interface")
            .await
            .map_err(|e| e.to_string())?;
        result.radios.push(Radio {
            path: path.to_string(),
            name,
        });
        if !enabled {
            continue;
        }
        let state: u32 = device
            .get_property("State")
            .await
            .map_err(|e| e.to_string())?;
        let wireless = proxy(connection, NM, path.as_str(), WIRELESS).await?;
        let active: OwnedObjectPath = wireless
            .get_property("ActiveAccessPoint")
            .await
            .map_err(|e| e.to_string())?;
        let aps: Vec<OwnedObjectPath> = wireless
            .call("GetAllAccessPoints", &())
            .await
            .map_err(|e| e.to_string())?;
        for ap_path in aps {
            let ap = proxy(connection, NM, ap_path.as_str(), AP).await?;
            let ssid: Vec<u8> = ap.get_property("Ssid").await.map_err(|e| e.to_string())?;
            if ssid.is_empty() {
                continue;
            }
            let strength = ap
                .get_property("Strength")
                .await
                .map_err(|e| e.to_string())?;
            let flags: u32 = ap.get_property("Flags").await.map_err(|e| e.to_string())?;
            let wpa: u32 = ap
                .get_property("WpaFlags")
                .await
                .map_err(|e| e.to_string())?;
            let rsn: u32 = ap
                .get_property("RsnFlags")
                .await
                .map_err(|e| e.to_string())?;
            let auth = security(flags, wpa, rsn);
            let saved = result
                .saved
                .iter()
                .find(|s| s.ssid == ssid && compatible(s.security, auth, wpa | rsn));
            let network = Network {
                name: String::from_utf8_lossy(&ssid).into_owned(),
                saved: saved.map(|s| s.path.clone()),
                ssid,
                security: auth,
                strength,
                device: path.to_string(),
                ap: ap_path.to_string(),
                connected: active == ap_path && state == 100,
                connecting: active == ap_path && (40..100).contains(&state),
                hidden: false,
            };
            insert_network(&mut networks, network);
        }
    }
    result.networks = networks.into_values().collect();
    result.networks.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.connecting.cmp(&a.connecting))
            .then(b.strength.cmp(&a.strength))
            .then(a.name.cmp(&b.name))
    });
    Ok(result)
}

fn compatible(saved: Security, advertised: Security, auth: u32) -> bool {
    saved == advertised
        || saved == Security::Personal && advertised == Security::Sae
        || saved == Security::Sae && advertised == Security::Personal && auth & 0x400 != 0
}

fn insert_network(networks: &mut BTreeMap<(Vec<u8>, Security, String), Network>, network: Network) {
    // Keep adapters separate, and prefer the active AP over a stronger same-SSID AP.
    let key = (
        network.ssid.clone(),
        network.security,
        network.device.clone(),
    );
    if networks.get(&key).is_none_or(|old| {
        (network.connected, network.connecting, network.strength)
            > (old.connected, old.connecting, old.strength)
    }) {
        networks.insert(key, network);
    }
}

async fn profiles(connection: &Connection) -> Result<Vec<Saved>, String> {
    let settings = proxy(
        connection,
        NM,
        "/org/freedesktop/NetworkManager/Settings",
        SETTINGS,
    )
    .await?;
    let paths: Vec<OwnedObjectPath> = settings
        .call("ListConnections", &())
        .await
        .map_err(|e| e.to_string())?;
    let mut saved = Vec::new();
    for path in paths {
        let profile = proxy(connection, NM, path.as_str(), PROFILE).await?;
        let Ok(values) = profile.call::<_, _, Settings>("GetSettings", &()).await else {
            continue;
        };
        let Some(wireless) = values.get("802-11-wireless") else {
            continue;
        };
        let Some(ssid) = wireless
            .get("ssid")
            .and_then(|v| Vec::<u8>::try_from(v.try_clone().ok()?).ok())
        else {
            continue;
        };
        let key = values
            .get("802-11-wireless-security")
            .and_then(|s| s.get("key-mgmt"))
            .and_then(|v| <&str>::try_from(v).ok());
        let security = match key {
            None => Security::Open,
            Some("wpa-psk") => Security::Personal,
            Some("sae") => Security::Sae,
            Some("owe") => Security::Owe,
            Some("wpa-eap" | "wpa-eap-suite-b-192" | "ieee8021x") => Security::Enterprise,
            _ => Security::Unsupported,
        };
        let name = values
            .get("connection")
            .and_then(|s| s.get("id"))
            .and_then(|v| <&str>::try_from(v).ok())
            .map(str::to_owned)
            .unwrap_or_else(|| String::from_utf8_lossy(&ssid).into_owned());
        saved.push(Saved {
            path: path.to_string(),
            name,
            ssid,
            security,
        });
    }
    saved.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(saved)
}

pub(crate) async fn power(connection: &Connection, enabled: bool) -> Result<(), String> {
    proxy(connection, NM, ROOT, NM)
        .await?
        .set_property("WirelessEnabled", enabled)
        .await
        .map_err(|e| e.to_string())
}

pub(crate) async fn scan(connection: &Connection) -> Result<(), String> {
    let root = proxy(connection, NM, ROOT, NM).await?;
    let devices: Vec<OwnedObjectPath> = root
        .call("GetDevices", &())
        .await
        .map_err(|e| e.to_string())?;
    let mut found = false;
    for path in devices {
        let device = proxy(connection, NM, path.as_str(), DEVICE).await?;
        if device.get_property::<u32>("DeviceType").await.unwrap_or(0) != 2 {
            continue;
        }
        found = true;
        proxy(connection, NM, path.as_str(), WIRELESS)
            .await?
            .call::<_, _, ()>("RequestScan", &(HashMap::<String, OwnedValue>::new(),))
            .await
            .map_err(|e| e.to_string())?;
    }
    if found {
        Ok(())
    } else {
        Err("No Wi-Fi adapter is available.".into())
    }
}

pub(crate) fn valid_password(security: Security, password: &str) -> bool {
    match security {
        Security::Personal => {
            (8..=63).contains(&password.len())
                || (password.len() == 64 && password.bytes().all(|b| b.is_ascii_hexdigit()))
        }
        Security::Sae => !password.is_empty(),
        _ => true,
    }
}

fn settings<'a>(
    network: &'a Network,
    password: &'a str,
) -> Result<HashMap<&'static str, HashMap<&'static str, Value<'a>>>, String> {
    if !network.security.can_create() {
        return Err(
            "This network needs an existing NetworkManager profile with its enterprise or legacy security settings."
                .into(),
        );
    }
    if !valid_password(network.security, password) {
        return Err("WPA passwords need 8–63 characters, or 64 hexadecimal digits.".into());
    }
    let mut settings = HashMap::from([
        (
            "connection",
            HashMap::from([
                ("id", Value::from(network.name.as_str())),
                ("type", Value::from("802-11-wireless")),
            ]),
        ),
        (
            "802-11-wireless",
            HashMap::from([
                ("ssid", Value::from(network.ssid.as_slice())),
                ("hidden", Value::from(network.hidden)),
            ]),
        ),
        ("ipv4", HashMap::from([("method", Value::from("auto"))])),
        ("ipv6", HashMap::from([("method", Value::from("auto"))])),
    ]);
    let key = match network.security {
        Security::Personal => Some("wpa-psk"),
        Security::Sae => Some("sae"),
        Security::Owe => Some("owe"),
        _ => None,
    };
    if let Some(key) = key {
        let mut security = HashMap::from([("key-mgmt", Value::from(key))]);
        if network.security.password() {
            security.insert("psk", Value::from(password));
        }
        settings.insert("802-11-wireless-security", security);
    }
    Ok(settings)
}

pub(crate) async fn connect(
    connection: &Connection,
    network: &Network,
    password: &str,
) -> Result<(), String> {
    let root = proxy(connection, NM, ROOT, NM).await?;
    let device = OwnedObjectPath::try_from(network.device.as_str()).map_err(|e| e.to_string())?;
    let ap = OwnedObjectPath::try_from(network.ap.as_str()).map_err(|e| e.to_string())?;
    let active: OwnedObjectPath = if let Some(saved) = &network.saved {
        if !password.is_empty() && network.security.password() {
            if !valid_password(network.security, password) {
                return Err("Invalid network password.".into());
            }
            let profile = proxy(connection, NM, saved, PROFILE).await?;
            let previous: Settings = profile
                .call("GetSettings", &())
                .await
                .map_err(|e| e.to_string())?;
            let mut values: HashMap<String, HashMap<String, Value<'_>>> = previous
                .into_iter()
                .map(|(section, values)| {
                    (
                        section,
                        values
                            .into_iter()
                            .map(|(key, value)| (key, Value::from(value)))
                            .collect(),
                    )
                })
                .collect();
            let security = values.entry("802-11-wireless-security".into()).or_default();
            security.insert("psk".into(), Value::from(password));
            security.insert("psk-flags".into(), Value::from(0u32));
            profile
                .call::<_, _, ()>("Update", &(values,))
                .await
                .map_err(|e| e.to_string())?;
        }
        let saved = OwnedObjectPath::try_from(saved.as_str()).map_err(|e| e.to_string())?;
        root.call("ActivateConnection", &(saved, &device, &ap))
            .await
            .map_err(|e| e.to_string())?
    } else {
        let values = settings(network, password)?;
        let (_, active): (OwnedObjectPath, OwnedObjectPath) = root
            .call("AddAndActivateConnection", &(values, &device, &ap))
            .await
            .map_err(|e| e.to_string())?;
        active
    };
    // Activation's reply only means accepted. Wait for its actual result.
    let active = proxy(
        connection,
        NM,
        active.as_str(),
        "org.freedesktop.NetworkManager.Connection.Active",
    )
    .await?;
    for _ in 0..90 {
        match active
            .get_property::<u32>("State")
            .await
            .map_err(|e| e.to_string())?
        {
            2 => return Ok(()),
            3 | 4 => {
                return Err(
                    "The network connection failed. Check the password and try again.".into(),
                );
            }
            _ => tokio::time::sleep(std::time::Duration::from_secs(1)).await,
        }
    }
    Err("The network did not finish connecting. Check its status before trying again.".into())
}

pub(crate) async fn disconnect(connection: &Connection, device: &str) -> Result<(), String> {
    proxy(connection, NM, device, DEVICE)
        .await?
        .call::<_, _, ()>("Disconnect", &())
        .await
        .map_err(|e| e.to_string())
}

pub(crate) async fn forget(connection: &Connection, profile: &str) -> Result<(), String> {
    proxy(connection, NM, profile, PROFILE)
        .await?
        .call::<_, _, ()>("Delete", &())
        .await
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network() -> Network {
        Network {
            ssid: b"test".to_vec(),
            name: "test".into(),
            security: Security::Personal,
            strength: 10,
            device: "/device/1".into(),
            ap: "/ap/1".into(),
            saved: None,
            connected: false,
            connecting: false,
            hidden: false,
        }
    }

    #[test]
    fn security_and_credentials() {
        assert_eq!(security(0, 0, 0), Security::Open);
        assert_eq!(security(1, 0x100, 0x500), Security::Personal);
        assert_eq!(security(1, 0, 0x400), Security::Sae);
        assert_eq!(security(1, 0, 0x200), Security::Enterprise);
        assert_eq!(security(0, 0, 0x800), Security::Owe);
        assert!(!valid_password(Security::Personal, "short"));
        assert!(valid_password(Security::Personal, &"a".repeat(64)));
        assert!(!valid_password(Security::Personal, &"z".repeat(64)));
        assert!(valid_password(Security::Sae, "short"));
        assert!(compatible(Security::Sae, Security::Personal, 0x500));
        assert!(!compatible(Security::Sae, Security::Personal, 0x100));
        assert!(compatible(Security::Personal, Security::Sae, 0x400));
        assert_eq!(security(1, 0, 0x2000), Security::Enterprise);
    }

    #[test]
    fn active_ap_wins_deduplication() {
        let mut networks = BTreeMap::new();
        let mut active = network();
        active.connected = true;
        insert_network(&mut networks, active);
        let mut strong = network();
        strong.strength = 99;
        insert_network(&mut networks, strong.clone());
        assert!(networks.values().next().unwrap().connected);
        strong.security = Security::Open;
        insert_network(&mut networks, strong);
        assert_eq!(networks.len(), 2);
    }

    #[test]
    fn secrets_only_go_to_secure_profiles() {
        let mut network = network();
        network.hidden = true;
        let values = settings(&network, "password").unwrap();
        assert_eq!(
            values["802-11-wireless-security"]["psk"],
            Value::from("password")
        );
        assert_eq!(values["802-11-wireless"]["hidden"], Value::from(true));
        network.security = Security::Open;
        assert!(
            !settings(&network, "")
                .unwrap()
                .contains_key("802-11-wireless-security")
        );
        network.security = Security::Enterprise;
        assert!(settings(&network, "password").is_err());
    }
}
