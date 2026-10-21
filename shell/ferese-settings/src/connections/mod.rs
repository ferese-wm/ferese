//! Connections are managed by NetworkManager and BlueZ, never by the KDL store.
mod agent;
mod bluetooth;
mod bluetooth_ui;

#[cfg(test)]
mod contracts;
mod controller;
mod pairing_ui;
mod ui;
mod wifi;
mod wifi_ui;

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

pub(crate) use ui::Input;
pub(crate) use wifi::{Network, Security};
use zbus::{Connection, Proxy};
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Tab {
    #[default]
    Wifi,
    Bluetooth,
}

#[derive(Clone, Debug)]
pub(crate) struct Snapshot {
    pub wifi: Result<wifi::Wifi, String>,
    pub bluetooth: Result<bluetooth::Bluetooth, String>,
}

pub(crate) struct State {
    pub client: Option<Arc<Client>>,
    pub snapshot: Option<Snapshot>,
    pub tab: Tab,
    pub loading: bool,
    pub busy: Option<&'static str>,
    pub error: Option<String>,
    pub selected: Option<Network>,
    pub password: Zeroizing<String>,
    pub hidden_name: String,
    pub hidden: bool,
    pub hidden_security: Security,
    pub adapter: Option<String>,
    pub prompt: Option<agent::Prompt>,
    pub pairing_input: Zeroizing<String>,
    pub forget: Option<Command>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            client: None,
            snapshot: None,
            tab: Tab::Wifi,
            loading: false,
            busy: None,
            error: None,
            selected: None,
            password: Zeroizing::new(String::new()),
            hidden_name: String::new(),
            hidden: false,
            hidden_security: Security::Personal,
            adapter: None,
            prompt: None,
            pairing_input: Zeroizing::new(String::new()),
            forget: None,
        }
    }
}

/// Debug deliberately omits credentials, including messages logged by the UI.
#[derive(Clone)]
pub(crate) enum Command {
    WifiPower(bool),
    WifiScan,
    WifiConnect(Network, Zeroizing<String>),
    WifiDisconnect(String),
    WifiForget(String),
    BluetoothPower(String, bool),
    BluetoothScan(String),
    BluetoothStop,
    BluetoothPair(String),
    BluetoothConnect(String),
    BluetoothDisconnect(String),
    BluetoothTrust(String, bool),
    BluetoothForget(String, String),
    CancelPairing,
}

impl Command {
    pub fn label(&self) -> &'static str {
        match self {
            Self::WifiPower(_) | Self::BluetoothPower(..) => "Updating radio…",
            Self::WifiScan => "Scanning for networks…",
            Self::WifiConnect(..) | Self::BluetoothConnect(_) => "Connecting…",
            Self::WifiDisconnect(_) | Self::BluetoothDisconnect(_) => "Disconnecting…",
            Self::WifiForget(_) | Self::BluetoothForget(..) => "Forgetting…",
            Self::BluetoothScan(_) => "Starting discovery…",
            Self::BluetoothStop => "Stopping discovery…",
            Self::BluetoothPair(_) => "Pairing…",
            Self::BluetoothTrust(..) => "Updating device…",
            Self::CancelPairing => "Canceling pairing…",
        }
    }
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

pub(crate) struct Client {
    connection: Connection,
    runtime: tokio::runtime::Handle,
    epoch: std::sync::atomic::AtomicU64,
    closed: std::sync::atomic::AtomicBool,
    agent: agent::Pairing,
    registered: tokio::sync::Mutex<Option<String>>,
    discovery: Arc<std::sync::Mutex<std::collections::BTreeMap<String, std::time::Instant>>>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ConnectionsClient")
    }
}

impl Client {
    pub async fn new() -> Result<Arc<Self>, String> {
        let connection = zbus::connection::Builder::system()
            .map_err(|e| e.to_string())?
            .method_timeout(Duration::from_secs(120))
            .build()
            .await
            .map_err(|e| e.to_string())?;
        Ok(Self::with_connection(connection))
    }

    fn with_connection(connection: Connection) -> Arc<Self> {
        Arc::new(Self {
            connection,
            runtime: tokio::runtime::Handle::current(),
            epoch: std::sync::atomic::AtomicU64::new(0),
            closed: std::sync::atomic::AtomicBool::new(false),
            agent: agent::Pairing::default(),
            registered: tokio::sync::Mutex::new(None),
            discovery: Arc::default(),
        })
    }

    pub async fn snapshot(&self) -> Snapshot {
        // Isolate missing services: a Bluetooth failure must not hide Wi-Fi.
        let (wifi, bluetooth) = tokio::join!(
            bounded(wifi::read(&self.connection)),
            bounded(bluetooth::read(&self.connection)),
        );
        Snapshot { wifi, bluetooth }
    }

    pub fn prompt(&self) -> Option<agent::Prompt> {
        self.agent.prompt()
    }

    pub fn reply(&self, id: u64, value: Option<Zeroizing<String>>) {
        self.agent.reply(id, value);
    }

    pub fn epoch(&self) -> u64 {
        self.epoch.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg(test)]
    pub async fn execute(&self, command: Command) -> Result<(), String> {
        self.execute_checked(command, self.epoch()).await
    }

    pub async fn execute_checked(&self, command: Command, epoch: u64) -> Result<(), String> {
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) || self.epoch() != epoch {
            return Err("This connection request was canceled.".into());
        }
        match command {
            Command::WifiPower(on) => wifi::power(&self.connection, on).await,
            Command::WifiScan => wifi::scan(&self.connection).await,
            Command::WifiConnect(network, password) => {
                wifi::connect(&self.connection, &network, &password).await
            }
            Command::WifiDisconnect(path) => wifi::disconnect(&self.connection, &path).await,
            Command::WifiForget(path) => wifi::forget(&self.connection, &path).await,
            Command::BluetoothPower(path, on) => {
                if !on {
                    self.stop_discovery().await?;
                }
                bluetooth::power(&self.connection, &path, on).await
            }
            Command::BluetoothScan(path) => self.discover(path, epoch).await,
            Command::BluetoothStop => self.stop_discovery().await,
            Command::BluetoothPair(path) => self.pair(&path, epoch).await,
            Command::BluetoothConnect(path) => {
                bluetooth::device_call(&self.connection, &path, "Connect").await
            }
            Command::BluetoothDisconnect(path) => {
                bluetooth::device_call(&self.connection, &path, "Disconnect").await
            }
            Command::BluetoothTrust(path, on) => {
                bluetooth::trust(&self.connection, &path, on).await
            }
            Command::BluetoothForget(adapter, path) => {
                bluetooth::forget(&self.connection, &adapter, &path).await
            }
            Command::CancelPairing => self.cancel_pairing().await,
        }
    }

    pub fn leave_now(&self) {
        self.epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.cleanup();
    }

    pub fn shutdown(&self) {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
        self.leave_now();
    }

    fn cleanup(&self) {
        let target = self.agent.cancel_local();
        let paths = std::mem::take(&mut *self.discovery.lock().unwrap());
        let connection = self.connection.clone();
        self.runtime.spawn(async move {
            if let Some(path) = target {
                let _ = bluetooth::device_call(&connection, &path, "CancelPairing").await;
            }
            for (path, _) in paths {
                let _ = bluetooth::adapter_call(&connection, &path, "StopDiscovery").await;
            }
        });
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.shutdown();
    }
}

async fn bounded<T>(
    future: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .map_err(|_| "The connection service did not respond. Try again.".to_owned())?
}

async fn proxy<'a>(
    connection: &'a Connection,
    destination: &'a str,
    path: &'a str,
    interface: &'a str,
) -> Result<Proxy<'a>, String> {
    Proxy::new(connection, destination, path, interface)
        .await
        .map_err(|e| e.to_string())
}
