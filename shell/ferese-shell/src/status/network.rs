//! Read NetworkManager directly; no scans or network changes are requested.
use zbus::blocking::Connection;
use zbus::zvariant::OwnedObjectPath;

use super::Network;

const SERVICE: &str = "org.freedesktop.NetworkManager";
const ROOT: &str = "/org/freedesktop/NetworkManager";

fn property<T: TryFrom<zbus::zvariant::OwnedValue>>(
    connection: &Connection,
    path: &str,
    interface: &str,
    name: &str,
) -> zbus::Result<T>
where
    T::Error: Into<zbus::Error>,
{
    // Explicit Get avoids creating short-lived property-cache subscriptions.
    let value: zbus::zvariant::OwnedValue = connection
        .call_method(
            Some(SERVICE),
            path,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(interface, name),
        )?
        .body()
        .deserialize()?;
    value.try_into().map_err(Into::into)
}

pub(super) fn read(connection: &Connection) -> zbus::Result<Option<Network>> {
    let devices: Vec<OwnedObjectPath> = property(connection, ROOT, SERVICE, "Devices")?;
    let mut wifi = Vec::new();
    for device in devices {
        let kind: u32 = property(
            connection,
            device.as_str(),
            "org.freedesktop.NetworkManager.Device",
            "DeviceType",
        )?;
        if kind == 2 {
            wifi.push(device);
        }
    }
    if wifi.is_empty() {
        return Ok(None);
    }
    // Stable selection when several Wi-Fi adapters have active connections.
    wifi.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    let enabled = property(connection, ROOT, SERVICE, "WirelessEnabled")?;
    let mut result = Network {
        enabled,
        connection: None,
        signal: 0,
    };
    if enabled {
        for device in wifi {
            let ap: OwnedObjectPath = property(
                connection,
                device.as_str(),
                "org.freedesktop.NetworkManager.Device.Wireless",
                "ActiveAccessPoint",
            )?;
            if ap.as_str() == "/" {
                continue;
            }
            let ssid: Vec<u8> = property(
                connection,
                ap.as_str(),
                "org.freedesktop.NetworkManager.AccessPoint",
                "Ssid",
            )?;
            let strength: u8 = property(
                connection,
                ap.as_str(),
                "org.freedesktop.NetworkManager.AccessPoint",
                "Strength",
            )?;
            result.connection = Some(String::from_utf8_lossy(&ssid).into_owned());
            result.signal = strength.min(100);
            break;
        }
    }
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    #[derive(Default)]
    struct State {
        wifi: bool,
        enabled: bool,
        active: bool,
        strength: u8,
    }
    struct Manager(Arc<Mutex<State>>);
    #[zbus::interface(name = "org.freedesktop.NetworkManager")]
    impl Manager {
        #[zbus(property)]
        fn devices(&self) -> Vec<OwnedObjectPath> {
            vec![OwnedObjectPath::try_from("/device").unwrap()]
        }
        #[zbus(property)]
        fn wireless_enabled(&self) -> bool {
            self.0.lock().unwrap().enabled
        }
    }
    struct Device(Arc<Mutex<State>>);
    #[zbus::interface(name = "org.freedesktop.NetworkManager.Device")]
    impl Device {
        #[zbus(property)]
        fn device_type(&self) -> u32 {
            if self.0.lock().unwrap().wifi { 2 } else { 1 }
        }
    }
    struct Wireless(Arc<Mutex<State>>);
    #[zbus::interface(name = "org.freedesktop.NetworkManager.Device.Wireless")]
    impl Wireless {
        #[zbus(property)]
        fn active_access_point(&self) -> OwnedObjectPath {
            OwnedObjectPath::try_from(if self.0.lock().unwrap().active {
                "/ap"
            } else {
                "/"
            })
            .unwrap()
        }
    }
    struct Ap(Arc<Mutex<State>>);
    #[zbus::interface(name = "org.freedesktop.NetworkManager.AccessPoint")]
    impl Ap {
        #[zbus(property)]
        fn ssid(&self) -> Vec<u8> {
            b"wifi:with\\escapes".to_vec()
        }
        #[zbus(property)]
        fn strength(&self) -> u8 {
            self.0.lock().unwrap().strength
        }
    }
    #[test]
    #[ignore = "requires dbus-daemon; uses a private test bus"]
    fn fresh_network_state_and_service_recovery() {
        let bus = crate::status::tests::TestBus::new();
        let state = Arc::new(Mutex::new(State::default()));
        let server = zbus::blocking::connection::Builder::address(bus.address.as_str())
            .unwrap()
            .name(SERVICE)
            .unwrap()
            .serve_at(ROOT, Manager(state.clone()))
            .unwrap()
            .serve_at("/device", Device(state.clone()))
            .unwrap()
            .serve_at("/device", Wireless(state.clone()))
            .unwrap()
            .serve_at("/ap", Ap(state.clone()))
            .unwrap()
            .build()
            .unwrap();
        let client = bus.connect();
        assert!(read(&client).unwrap().is_none());
        state.lock().unwrap().wifi = true;
        let value = read(&client).unwrap().unwrap();
        assert!(!value.enabled && value.connection.is_none());
        state.lock().unwrap().enabled = true;
        assert!(read(&client).unwrap().unwrap().connection.is_none());
        {
            let mut s = state.lock().unwrap();
            s.active = true;
            s.strength = 73;
        }
        let value = read(&client).unwrap().unwrap();
        assert_eq!(value.connection.as_deref(), Some("wifi:with\\escapes"));
        assert_eq!(value.signal, 73);
        state.lock().unwrap().strength = 250;
        assert_eq!(read(&client).unwrap().unwrap().signal, 100);
        server.release_name(SERVICE).unwrap();
        assert!(read(&client).is_err());
        server.request_name(SERVICE).unwrap();
        assert_eq!(read(&client).unwrap().unwrap().signal, 100);
        server.object_server().remove::<Ap, _>("/ap").unwrap();
        assert!(read(&client).is_err());
        state.lock().unwrap().active = false;
        assert!(read(&client).unwrap().unwrap().connection.is_none());
    }
}
