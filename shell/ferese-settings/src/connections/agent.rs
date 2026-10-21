//! A caller-owned BlueZ agent. Only the device being paired may ask for input.
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::oneshot;
use zbus::message::Header;
use zbus::zvariant::OwnedObjectPath;
use zeroize::Zeroizing;

use super::{Client, bluetooth, proxy};

const PATH: &str = "/dev/ferese/Settings/BluetoothAgent";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Pin,
    Passkey,
    Confirm(u32),
    Authorize,
    Service(String),
    Display(String),
}

pub(super) fn valid_answer(kind: &Kind, value: &str) -> bool {
    match kind {
        Kind::Pin => !value.is_empty() && value.chars().count() <= 16,
        Kind::Passkey => {
            !value.is_empty() && value.len() <= 6 && value.bytes().all(|b| b.is_ascii_digit())
        }
        Kind::Display(_) => false,
        _ => true,
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Prompt {
    pub id: u64,
    pub device: String,
    pub kind: Kind,
}

impl fmt::Debug for Prompt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BluetoothPairingPrompt")
    }
}

#[derive(Default)]
struct Pending {
    target: Option<String>,
    owner: Option<String>,
    serial: u64,
    generation: u64,
    prompt: Option<Prompt>,
    reply: Option<oneshot::Sender<Option<Zeroizing<String>>>>,
}

#[derive(Clone, Default)]
pub(crate) struct Pairing(Arc<Mutex<Pending>>);

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.bluez.Error")]
enum Error {
    Rejected(String),
    Canceled(String),
}

impl Pairing {
    fn begin(&self, target: &str) -> Result<u64, String> {
        let mut state = self.0.lock().unwrap();
        if state.target.is_some() {
            return Err("Another device is already pairing.".into());
        }
        state.generation = state.generation.wrapping_add(1);
        state.target = Some(target.to_owned());
        Ok(state.generation)
    }

    fn finish(&self, generation: u64) {
        let mut state = self.0.lock().unwrap();
        if state.generation == generation {
            state.target = None;
            state.prompt = None;
            state.reply = None;
        }
    }

    pub fn cancel_local(&self) -> Option<String> {
        let mut state = self.0.lock().unwrap();
        state.generation = state.generation.wrapping_add(1);
        state.prompt = None;
        state.reply = None;
        state.target.take()
    }

    pub fn prompt(&self) -> Option<Prompt> {
        self.0.lock().unwrap().prompt.clone()
    }

    pub fn reply(&self, id: u64, value: Option<Zeroizing<String>>) {
        let mut state = self.0.lock().unwrap();
        if state.prompt.as_ref().is_some_and(|p| p.id == id) {
            if let Some(reply) = state.reply.take() {
                let _ = reply.send(value);
            }
            state.prompt = None;
        }
    }

    fn check(state: &Pending, device: &str, sender: Option<&str>) -> Result<(), Error> {
        if state.target.as_deref() != Some(device)
            || sender.is_none()
            || state.owner.as_deref() != sender
        {
            Err(Error::Rejected("No matching pairing request".into()))
        } else {
            Ok(())
        }
    }

    async fn ask(
        &self,
        device: OwnedObjectPath,
        kind: Kind,
        sender: Option<&str>,
    ) -> Result<Zeroizing<String>, Error> {
        let (id, rx) = {
            let mut state = self.0.lock().unwrap();
            Self::check(&state, device.as_str(), sender)?;
            if state.reply.is_some() {
                return Err(Error::Rejected(
                    "A pairing question is already pending".into(),
                ));
            }
            state.serial = state.serial.wrapping_add(1);
            let id = state.serial;
            let (tx, rx) = oneshot::channel();
            state.prompt = Some(Prompt {
                id,
                device: device.to_string(),
                kind,
            });
            state.reply = Some(tx);
            (id, rx)
        };
        let answer = tokio::time::timeout(Duration::from_secs(90), rx).await;
        self.reply(id, None);
        answer
            .ok()
            .and_then(Result::ok)
            .flatten()
            .ok_or_else(|| Error::Canceled("Pairing canceled or timed out".into()))
    }

    fn display(
        &self,
        device: OwnedObjectPath,
        text: String,
        sender: Option<&str>,
    ) -> Result<(), Error> {
        let mut state = self.0.lock().unwrap();
        Self::check(&state, device.as_str(), sender)?;
        state.serial = state.serial.wrapping_add(1);
        state.prompt = Some(Prompt {
            id: state.serial,
            device: device.to_string(),
            kind: Kind::Display(text),
        });
        Ok(())
    }

    fn dismiss(&self, sender: Option<&str>) -> Result<(), Error> {
        let mut state = self.0.lock().unwrap();
        if sender.is_none() || state.owner.as_deref() != sender {
            return Err(Error::Rejected("Unknown sender".into()));
        }
        state.prompt = None;
        state.reply = None;
        Ok(())
    }
}

#[zbus::interface(name = "org.bluez.Agent1")]
impl Pairing {
    async fn request_pin_code(
        &self,
        device: OwnedObjectPath,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<String, Error> {
        let answer = self
            .ask(device, Kind::Pin, header.sender().map(|s| s.as_str()))
            .await?;
        if !valid_answer(&Kind::Pin, &answer) {
            return Err(Error::Rejected("PIN must have 1–16 characters".into()));
        }
        Ok(answer.to_string())
    }

    async fn request_passkey(
        &self,
        device: OwnedObjectPath,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<u32, Error> {
        let answer = self
            .ask(device, Kind::Passkey, header.sender().map(|s| s.as_str()))
            .await?;
        if !valid_answer(&Kind::Passkey, &answer) {
            return Err(Error::Rejected("Passkey must have up to six digits".into()));
        }
        answer
            .parse::<u32>()
            .ok()
            .filter(|v| *v <= 999_999)
            .ok_or_else(|| Error::Rejected("Invalid passkey".into()))
    }

    async fn request_confirmation(
        &self,
        device: OwnedObjectPath,
        passkey: u32,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), Error> {
        self.ask(
            device,
            Kind::Confirm(passkey),
            header.sender().map(|s| s.as_str()),
        )
        .await
        .map(|_| ())
    }

    async fn request_authorization(
        &self,
        device: OwnedObjectPath,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), Error> {
        self.ask(device, Kind::Authorize, header.sender().map(|s| s.as_str()))
            .await
            .map(|_| ())
    }

    async fn authorize_service(
        &self,
        device: OwnedObjectPath,
        uuid: String,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), Error> {
        self.ask(
            device,
            Kind::Service(uuid),
            header.sender().map(|s| s.as_str()),
        )
        .await
        .map(|_| ())
    }

    fn display_pin_code(
        &self,
        device: OwnedObjectPath,
        pincode: String,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), Error> {
        self.display(
            device,
            format!("Type {pincode} on the device, then press Enter."),
            header.sender().map(|s| s.as_str()),
        )
    }

    fn display_passkey(
        &self,
        device: OwnedObjectPath,
        passkey: u32,
        entered: u16,
        #[zbus(header)] header: Header<'_>,
    ) -> Result<(), Error> {
        self.display(
            device,
            format!("Type {passkey:06} on the device, then press Enter. {entered} digits entered."),
            header.sender().map(|s| s.as_str()),
        )
    }

    fn cancel(&self, #[zbus(header)] header: Header<'_>) -> Result<(), Error> {
        self.dismiss(header.sender().map(|s| s.as_str()))
    }

    fn release(&self, #[zbus(header)] header: Header<'_>) -> Result<(), Error> {
        self.dismiss(header.sender().map(|s| s.as_str()))?;
        self.cancel_local();
        self.0.lock().unwrap().owner = None;
        Ok(())
    }
}

impl Client {
    async fn register_agent(&self) -> Result<(), String> {
        let owner: String = proxy(
            &self.connection,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
        )
        .await?
        .call("GetNameOwner", &("org.bluez",))
        .await
        .map_err(|e| e.to_string())?;
        let mut registered = self.registered.lock().await;
        if registered.as_ref() == Some(&owner)
            && self.agent.0.lock().unwrap().owner.as_ref() == Some(&owner)
        {
            return Ok(());
        }
        self.agent.0.lock().unwrap().owner = Some(owner.clone());
        self.connection
            .object_server()
            .at(PATH, self.agent.clone())
            .await
            .map_err(|e| e.to_string())?;
        let path = OwnedObjectPath::try_from(PATH).unwrap();
        proxy(
            &self.connection,
            "org.bluez",
            "/org/bluez",
            "org.bluez.AgentManager1",
        )
        .await?
        .call::<_, _, ()>("RegisterAgent", &(path, "KeyboardDisplay"))
        .await
        .map_err(|e| e.to_string())?;
        *registered = Some(owner);
        Ok(())
    }

    pub(crate) async fn pair(&self, path: &str, epoch: u64) -> Result<(), String> {
        let generation = self.agent.begin(path)?;
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) || self.epoch() != epoch {
            self.agent.finish(generation);
            return Err("Pairing canceled.".into());
        }
        let result = async {
            self.register_agent().await?;
            if self.agent.0.lock().unwrap().generation != generation {
                return Err("Pairing canceled.".into());
            }
            bluetooth::device_call(&self.connection, path, "Pair").await
        }
        .await;
        let still_active = self.agent.0.lock().unwrap().generation == generation;
        self.agent.finish(generation);
        if !still_active {
            return Err("Pairing canceled.".into());
        }
        result?;
        bluetooth::trust(&self.connection, path, true).await?;
        bluetooth::device_call(&self.connection, path, "Connect").await
    }

    pub(crate) async fn cancel_pairing(&self) -> Result<(), String> {
        if let Some(path) = self.agent.cancel_local() {
            bluetooth::device_call(&self.connection, &path, "CancelPairing").await
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stale_answers_and_unknown_devices_cannot_approve_pairing() {
        let pairing = Pairing::default();
        pairing.0.lock().unwrap().owner = Some(":1.2".into());
        let generation = pairing.begin("/device/1").unwrap();
        let path = OwnedObjectPath::try_from("/device/2").unwrap();
        assert!(
            pairing
                .ask(path, Kind::Authorize, Some(":1.2"))
                .await
                .is_err()
        );
        let path = OwnedObjectPath::try_from("/device/1").unwrap();
        assert!(
            pairing
                .ask(path.clone(), Kind::Authorize, Some(":1.9"))
                .await
                .is_err()
        );
        let other = pairing.clone();
        let task =
            tokio::spawn(async move { other.ask(path, Kind::Confirm(123456), Some(":1.2")).await });
        tokio::task::yield_now().await;
        let id = pairing.prompt().unwrap().id;
        pairing.reply(id.wrapping_sub(1), Some(Zeroizing::new(String::new())));
        assert!(pairing.prompt().is_some());
        pairing.reply(id, Some(Zeroizing::new(String::new())));
        assert!(task.await.unwrap().is_ok());
        pairing.finish(generation);
        assert!(pairing.prompt().is_none());
    }

    #[tokio::test]
    async fn cancel_closes_the_question_and_old_completion_cannot_clear_new_pairing() {
        let pairing = Pairing::default();
        pairing.0.lock().unwrap().owner = Some(":1.2".into());
        let old = pairing.begin("/device/1").unwrap();
        let other = pairing.clone();
        let task = tokio::spawn(async move {
            other
                .ask(
                    OwnedObjectPath::try_from("/device/1").unwrap(),
                    Kind::Pin,
                    Some(":1.2"),
                )
                .await
        });
        tokio::task::yield_now().await;
        assert!(pairing.prompt().is_some());
        pairing.cancel_local();
        assert!(task.await.unwrap().is_err());
        pairing.begin("/device/2").unwrap();
        pairing.finish(old);
        assert_eq!(
            pairing.0.lock().unwrap().target.as_deref(),
            Some("/device/2")
        );
    }
}
