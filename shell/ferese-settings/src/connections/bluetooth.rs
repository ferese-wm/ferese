use std::collections::HashMap;
use std::time::Duration;

use zbus::Connection;
use zbus::fdo::ManagedObjects;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use super::{Client, proxy};

const BLUEZ: &str = "org.bluez";
const ADAPTER: &str = "org.bluez.Adapter1";
const DEVICE: &str = "org.bluez.Device1";

#[derive(Clone, Debug)]
pub(crate) struct Adapter {
    pub path: String,
    pub name: String,
    pub powered: bool,
    pub discovering: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Device {
    pub path: String,
    pub adapter: String,
    pub name: String,
    pub address: String,
    pub icon: String,
    pub connected: bool,
    pub paired: bool,
    pub trusted: bool,
    pub battery: Option<u8>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Bluetooth {
    pub adapters: Vec<Adapter>,
    pub devices: Vec<Device>,
}

pub(crate) async fn read(connection: &Connection) -> Result<Bluetooth, String> {
    let objects: ManagedObjects =
        proxy(connection, BLUEZ, "/", "org.freedesktop.DBus.ObjectManager")
            .await?
            .call("GetManagedObjects", &())
            .await
            .map_err(|e| e.to_string())?;
    Ok(snapshot(&objects))
}

fn string(properties: &HashMap<String, OwnedValue>, key: &str) -> String {
    properties
        .get(key)
        .and_then(|v| <&str>::try_from(v).ok())
        .unwrap_or_default()
        .to_owned()
}

fn boolean(properties: &HashMap<String, OwnedValue>, key: &str) -> bool {
    properties
        .get(key)
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false)
}

fn snapshot(objects: &ManagedObjects) -> Bluetooth {
    let mut result = Bluetooth::default();
    for (path, interfaces) in objects {
        if let Some(p) = interfaces.get(ADAPTER) {
            result.adapters.push(Adapter {
                path: path.to_string(),
                name: string(p, "Alias"),
                powered: boolean(p, "Powered"),
                discovering: boolean(p, "Discovering"),
            });
        }
        if let Some(p) = interfaces.get(DEVICE) {
            let Some(adapter) = p
                .get("Adapter")
                .and_then(|v| <&zbus::zvariant::ObjectPath>::try_from(v).ok())
            else {
                continue;
            };
            let address = string(p, "Address");
            let alias = string(p, "Alias");
            result.devices.push(Device {
                path: path.to_string(),
                adapter: adapter.to_string(),
                name: if alias.is_empty() {
                    address.clone()
                } else {
                    alias
                },
                address,
                icon: string(p, "Icon"),
                connected: boolean(p, "Connected"),
                paired: boolean(p, "Paired"),
                trusted: boolean(p, "Trusted"),
                battery: interfaces
                    .get("org.bluez.Battery1")
                    .and_then(|p| p.get("Percentage"))
                    .and_then(|v| u8::try_from(v).ok()),
            });
        }
    }
    result.adapters.sort_by(|a, b| a.path.cmp(&b.path));
    result.devices.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.paired.cmp(&a.paired))
            .then(a.name.cmp(&b.name))
    });
    result
}

pub(crate) async fn power(connection: &Connection, path: &str, on: bool) -> Result<(), String> {
    proxy(connection, BLUEZ, path, ADAPTER)
        .await?
        .set_property("Powered", on)
        .await
        .map_err(|e| e.to_string())
}

pub(crate) async fn adapter_call(
    connection: &Connection,
    path: &str,
    method: &str,
) -> Result<(), String> {
    proxy(connection, BLUEZ, path, ADAPTER)
        .await?
        .call::<_, _, ()>(method, &())
        .await
        .map_err(|e| e.to_string())
}

pub(crate) async fn device_call(
    connection: &Connection,
    path: &str,
    method: &str,
) -> Result<(), String> {
    proxy(connection, BLUEZ, path, DEVICE)
        .await?
        .call::<_, _, ()>(method, &())
        .await
        .map_err(|e| e.to_string())
}

pub(crate) async fn trust(connection: &Connection, path: &str, on: bool) -> Result<(), String> {
    proxy(connection, BLUEZ, path, DEVICE)
        .await?
        .set_property("Trusted", on)
        .await
        .map_err(|e| e.to_string())
}

pub(crate) async fn forget(
    connection: &Connection,
    adapter: &str,
    path: &str,
) -> Result<(), String> {
    let path = OwnedObjectPath::try_from(path).map_err(|e| e.to_string())?;
    proxy(connection, BLUEZ, adapter, ADAPTER)
        .await?
        .call::<_, _, ()>("RemoveDevice", &(path,))
        .await
        .map_err(|e| e.to_string())
}

impl Client {
    pub(crate) async fn discover(&self, path: String, epoch: u64) -> Result<(), String> {
        // BlueZ discovery is reference counted per D-Bus caller. Never stop
        // discovery owned by another app, and never acquire a lease twice.
        if self.discovery.lock().unwrap().contains_key(&path) {
            return Ok(());
        }
        adapter_call(&self.connection, &path, "StartDiscovery").await?;
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) || self.epoch() != epoch {
            let _ = adapter_call(&self.connection, &path, "StopDiscovery").await;
            return Err("Discovery canceled.".into());
        }
        let lease = std::time::Instant::now();
        self.discovery.lock().unwrap().insert(path.clone(), lease);
        let leases = self.discovery.clone();
        let connection = self.connection.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let owned = {
                let mut leases = leases.lock().unwrap();
                if leases.get(&path) == Some(&lease) {
                    leases.remove(&path);
                    true
                } else {
                    false
                }
            };
            if owned {
                let _ = adapter_call(&connection, &path, "StopDiscovery").await;
            }
        });
        Ok(())
    }

    pub(crate) async fn stop_discovery(&self) -> Result<(), String> {
        let paths = std::mem::take(&mut *self.discovery.lock().unwrap());
        let mut result = Ok(());
        for (path, _) in paths {
            if let Err(error) = adapter_call(&self.connection, &path, "StopDiscovery").await {
                result = Err(error);
            }
        }
        result
    }

    pub(crate) fn discovering(&self, path: &str) -> bool {
        self.discovery.lock().unwrap().contains_key(path)
    }
}

#[cfg(test)]
mod tests {
    use zbus::zvariant::Str;

    use super::*;

    #[test]
    fn parse_devices_and_keep_adapter_identity() {
        let adapter = OwnedObjectPath::try_from("/org/bluez/hci0").unwrap();
        let objects = HashMap::from([
            (
                adapter.clone(),
                HashMap::from([(
                    zbus::names::OwnedInterfaceName::try_from(ADAPTER).unwrap(),
                    HashMap::from([
                        ("Alias".into(), OwnedValue::from(Str::from("Laptop"))),
                        ("Powered".into(), OwnedValue::from(true)),
                    ]),
                )]),
            ),
            (
                OwnedObjectPath::try_from("/org/bluez/hci0/dev_1").unwrap(),
                HashMap::from([
                    (
                        zbus::names::OwnedInterfaceName::try_from(DEVICE).unwrap(),
                        HashMap::from([
                            (
                                "Adapter".into(),
                                OwnedValue::try_from(zbus::zvariant::Value::from(adapter)).unwrap(),
                            ),
                            ("Alias".into(), OwnedValue::from(Str::from("Headphones"))),
                            ("Connected".into(), OwnedValue::from(true)),
                            ("Paired".into(), OwnedValue::from(true)),
                        ]),
                    ),
                    (
                        zbus::names::OwnedInterfaceName::try_from("org.bluez.Battery1").unwrap(),
                        HashMap::from([("Percentage".into(), OwnedValue::from(80u8))]),
                    ),
                ]),
            ),
        ]);
        let snapshot = snapshot(&objects);
        assert!(snapshot.adapters[0].powered);
        assert!(snapshot.devices[0].connected);
        assert_eq!(snapshot.devices[0].adapter, "/org/bluez/hci0");
        assert_eq!(snapshot.devices[0].battery, Some(80));
    }
}
