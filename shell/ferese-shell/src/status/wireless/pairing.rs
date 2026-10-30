//! A per-request BlueZ agent; it never replaces the session's default agent.
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use cosmic::iced::futures::{
    SinkExt, Stream, StreamExt,
    channel::{mpsc, oneshot},
};
use zbus::zvariant::OwnedObjectPath;

use super::{BluetoothDevice, Password};
#[cfg(test)]
mod tests;

pub(super) const PATH: &str = "/dev/ferese/BluetoothAgent";
type Reply = Mutex<Option<oneshot::Sender<Option<Password>>>>;

#[derive(Clone, Debug)]
pub(crate) enum Prompt {
    Confirm(Option<u32>),
    Pin,
    Passkey,
    Display(String),
    Authorize,
}

#[derive(Clone)]
pub(crate) struct Request {
    pub id: u64,
    pub prompt: Prompt,
    response: Arc<Reply>,
}

impl std::fmt::Debug for Request {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PairingRequest([redacted])")
    }
}

impl Request {
    pub fn accepts(&self, value: &str) -> bool {
        match self.prompt {
            Prompt::Pin => !value.is_empty() && value.len() <= 16 && value.is_ascii(),
            Prompt::Passkey => {
                value.len() == 6 && value.bytes().all(|value| value.is_ascii_digit())
            }
            Prompt::Confirm(_) | Prompt::Authorize => true,
            Prompt::Display(_) => false,
        }
    }
    pub fn answer(&self, answer: Option<Password>) {
        if let Ok(mut response) = self.response.lock()
            && let Some(sender) = response.take()
        {
            let _ = sender.send(answer);
        }
    }
}

pub(crate) enum Update {
    Prompt(Request),
    Clear,
    Finished(Result<(), String>),
    CancellationFailed(String),
}

#[derive(Clone, Copy)]
#[repr(u8)]
pub(super) enum Phase {
    Preparing,
    Pairing,
    Connecting,
    Connected,
    TrustEnabled,
}

pub(super) fn cancel_operation(
    connection: &zbus::blocking::Connection,
    path: &str,
    phase: u8,
) -> Result<(), String> {
    if phase == Phase::Preparing as u8 {
        return Ok(());
    }
    if phase == Phase::Pairing as u8 {
        // Pair may have completed while cancellation crossed the bus.
        let _ = connection.call_method(
            Some("org.bluez"),
            path,
            Some("org.bluez.Device1"),
            "CancelPairing",
            &(),
        );
    }
    let restore_trust = if phase == Phase::TrustEnabled as u8 {
        let proxy = zbus::blocking::Proxy::new(connection, "org.bluez", path, "org.bluez.Device1")
            .map_err(|error| error.to_string())?;
        proxy.set_property("Trusted", false).map_err(|error| {
            format!("Could not restore Bluetooth trust after cancellation: {error}")
        })
    } else {
        Ok(())
    };
    match connection.call_method(
        Some("org.bluez"),
        path,
        Some("org.bluez.Device1"),
        "Disconnect",
        &(),
    ) {
        Ok(_) => restore_trust,
        Err(zbus::Error::MethodError(name, ..))
            if name.as_str() == "org.bluez.Error.NotConnected" =>
        {
            restore_trust
        }
        Err(error) => Err(format!(
            "Could not cancel the Bluetooth connection: {error}"
        )),
    }
}

async fn cancel_inflight(path: String, phase: Arc<AtomicU8>) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        if phase.load(Ordering::Acquire) == Phase::Preparing as u8 {
            return Ok(());
        }
        let connection = zbus::blocking::connection::Builder::system()
            .and_then(|builder| {
                builder
                    .method_timeout(std::time::Duration::from_secs(2))
                    .build()
            })
            .map_err(|error| error.to_string())?;
        cancel_operation(&connection, &path, phase.load(Ordering::Acquire))
    })
    .await
    .unwrap_or_else(|_| Err("Could not finish canceling the Bluetooth connection".into()))
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.bluez.Error")]
enum Error {
    Rejected(String),
    Canceled(String),
}

pub(super) struct Agent {
    pub(super) authority: Arc<OnceLock<String>>,
    pub(super) cancelled: Arc<AtomicBool>,
    pub(super) phase: Arc<AtomicU8>,
    device: String,
    sender: mpsc::UnboundedSender<Update>,
    pending: Mutex<Option<Weak<Reply>>>,
    sequence: AtomicU64,
}

impl Agent {
    fn verify(&self, header: &zbus::message::Header<'_>) -> Result<(), Error> {
        if self.authority.get().is_some_and(|owner| {
            header
                .sender()
                .is_some_and(|sender| sender.as_str() == owner)
        }) {
            Ok(())
        } else {
            Err(Error::Rejected(
                "Only BlueZ may request authentication".into(),
            ))
        }
    }
    fn check(&self, device: &OwnedObjectPath) -> Result<(), Error> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(Error::Canceled("Pairing was canceled".into()));
        }
        if device.as_str() == self.device {
            Ok(())
        } else {
            Err(Error::Rejected("This device was not selected".into()))
        }
    }
    async fn ask(&self, device: &OwnedObjectPath, prompt: Prompt) -> Result<Password, Error> {
        self.check(device)?;
        let (sender, receiver) = oneshot::channel();
        let response = Arc::new(Mutex::new(Some(sender)));
        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| Error::Canceled("Pairing was canceled".into()))?;
            if pending
                .as_ref()
                .and_then(Weak::upgrade)
                .is_some_and(|reply| reply.lock().is_ok_and(|reply| reply.is_some()))
            {
                return Err(Error::Rejected(
                    "A pairing question is already pending".into(),
                ));
            }
            *pending = Some(Arc::downgrade(&response));
        }
        self.sender
            .unbounded_send(Update::Prompt(Request {
                id: self.sequence.fetch_add(1, Ordering::Relaxed),
                prompt,
                response,
            }))
            .map_err(|_| Error::Canceled("Pairing was canceled".into()))?;
        let response = receiver
            .await
            .ok()
            .flatten()
            .ok_or_else(|| Error::Rejected("Pairing was canceled".into()))?;
        self.check(device)?;
        Ok(response)
    }
    fn cancel_request(&self) {
        self.cancelled.store(true, Ordering::Release);
        if let Ok(mut pending) = self.pending.lock()
            && let Some(reply) = pending.take().and_then(|reply| reply.upgrade())
            && let Ok(mut reply) = reply.lock()
        {
            reply.take();
        }
        let _ = self.sender.unbounded_send(Update::Clear);
    }
    fn display(&self, device: &OwnedObjectPath, code: String) -> Result<(), Error> {
        self.check(device)?;
        self.sender
            .unbounded_send(Update::Prompt(Request {
                id: self.sequence.fetch_add(1, Ordering::Relaxed),
                prompt: Prompt::Display(code),
                response: Arc::new(Mutex::new(None)),
            }))
            .map_err(|_| Error::Canceled("Pairing was canceled".into()))
    }
}

#[cfg(test)]
pub(crate) fn test_request(
    id: u64,
    prompt: Prompt,
) -> (Request, oneshot::Receiver<Option<Password>>) {
    let (sender, receiver) = oneshot::channel();
    (
        Request {
            id,
            prompt,
            response: Arc::new(Mutex::new(Some(sender))),
        },
        receiver,
    )
}

#[zbus::interface(name = "org.bluez.Agent1")]
impl Agent {
    fn release(&self, #[zbus(header)] header: zbus::message::Header<'_>) -> Result<(), Error> {
        self.verify(&header)?;
        self.cancel_request();
        Ok(())
    }
    async fn request_pin_code(
        &self,
        device: OwnedObjectPath,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> Result<String, Error> {
        self.verify(&header)?;
        let response = self.ask(&device, Prompt::Pin).await?;
        let pin = response.as_str();
        if pin.is_empty() || pin.len() > 16 || !pin.is_ascii() {
            return Err(Error::Rejected("Enter a PIN of 1 to 16 characters".into()));
        }
        Ok(pin.to_owned())
    }
    fn display_pin_code(
        &self,
        device: OwnedObjectPath,
        pincode: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> Result<(), Error> {
        self.verify(&header)?;
        self.display(&device, pincode)
    }
    async fn request_passkey(
        &self,
        device: OwnedObjectPath,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> Result<u32, Error> {
        self.verify(&header)?;
        let response = self.ask(&device, Prompt::Passkey).await?;
        if response.as_str().is_empty()
            || response.as_str().len() > 6
            || !response
                .as_str()
                .bytes()
                .all(|value| value.is_ascii_digit())
        {
            return Err(Error::Rejected("Enter a six-digit passkey".into()));
        }
        response
            .as_str()
            .parse::<u32>()
            .ok()
            .filter(|key| *key <= 999999)
            .ok_or_else(|| Error::Rejected("Enter a six-digit passkey".into()))
    }
    fn display_passkey(
        &self,
        device: OwnedObjectPath,
        passkey: u32,
        _entered: u16,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> Result<(), Error> {
        self.verify(&header)?;
        self.display(&device, format!("{passkey:06}"))
    }
    async fn request_confirmation(
        &self,
        device: OwnedObjectPath,
        passkey: u32,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> Result<(), Error> {
        self.verify(&header)?;
        self.ask(&device, Prompt::Confirm(Some(passkey)))
            .await
            .map(|_| ())
    }
    async fn request_authorization(
        &self,
        device: OwnedObjectPath,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> Result<(), Error> {
        self.verify(&header)?;
        self.ask(&device, Prompt::Confirm(None)).await.map(|_| ())
    }
    async fn authorize_service(
        &self,
        device: OwnedObjectPath,
        _uuid: String,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> Result<(), Error> {
        self.verify(&header)?;
        self.ask(&device, Prompt::Authorize).await.map(|_| ())
    }
    fn cancel(&self, #[zbus(header)] header: zbus::message::Header<'_>) -> Result<(), Error> {
        self.verify(&header)?;
        Agent::cancel_request(self);
        Ok(())
    }
}

pub(crate) fn connect(
    device: BluetoothDevice,
    cancel: oneshot::Receiver<()>,
) -> impl Stream<Item = Update> {
    cosmic::iced::stream::channel(4, async move |mut output: mpsc::Sender<Update>| {
        let (sender, mut requests) = mpsc::unbounded();
        let path = device.path.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let phase = Arc::new(AtomicU8::new(Phase::Preparing as u8));
        let agent = Agent {
            authority: Arc::new(OnceLock::new()),
            cancelled: cancelled.clone(),
            phase: phase.clone(),
            device: path.clone(),
            sender,
            pending: Mutex::new(None),
            sequence: AtomicU64::new(0),
        };
        let mut worker =
            tokio::task::spawn_blocking(move || super::connect_bluetooth(&device, agent));
        let mut cancel = cancel;
        // Forwarding a prompt must not block cancellation when the UI channel is full.
        let completed = tokio::select! {
            biased;
            _ = &mut cancel => None,
            result = &mut worker => Some(result),
            _ = async {
                while let Some(request) = requests.next().await {
                    if output.send(request).await.is_err() { break; }
                }
            } => None,
        };
        if let Some(result) = completed {
            let result =
                result.unwrap_or_else(|_| Err("Bluetooth connection did not finish".into()));
            let _ = output.send(Update::Finished(result)).await;
            return;
        }
        cancelled.store(true, Ordering::Release);
        let _ = output.try_send(Update::Clear);
        let _ = cancel_inflight(path.clone(), phase.clone()).await;
        // A Connect dispatched concurrently with cancellation must also be undone.
        let _ = worker.await;
        let update = match cancel_inflight(path, phase).await {
            Ok(()) => Update::Finished(Err("Pairing canceled".into())),
            Err(error) => Update::CancellationFailed(error),
        };
        let _ = output.send(update).await;
    })
}
