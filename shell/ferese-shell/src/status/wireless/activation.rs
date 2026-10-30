//! Retain failure transitions for one NetworkManager activation attempt.
use cosmic::iced::futures::{FutureExt, StreamExt};
use zbus::AsyncDrop;
use zbus::{MatchRule, MessageStream, blocking::Connection, message::Type};

use super::WifiError;

pub(super) struct Events {
    streams: Vec<MessageStream>,
    device: String,
    owns_device: bool,
    credentials_rejected: bool,
}

impl Events {
    pub fn subscribe(
        connection: &Connection,
        device: &str,
        authority: &str,
    ) -> Result<Self, WifiError> {
        let states = MatchRule::builder()
            .msg_type(Type::Signal)
            .sender(authority)
            .and_then(|rule| rule.member("StateChanged"))
            .map_err(|error| error.to_string())?
            .build();
        let ownership = MatchRule::builder()
            .msg_type(Type::Signal)
            .sender(authority)
            .and_then(|rule| rule.path(device))
            .and_then(|rule| rule.interface("org.freedesktop.DBus.Properties"))
            .and_then(|rule| rule.member("PropertiesChanged"))
            .map_err(|error| error.to_string())?
            .build();
        let mut events = Self {
            streams: Vec::with_capacity(2),
            device: device.into(),
            owns_device: false,
            credentials_rejected: false,
        };
        for rule in [states, ownership] {
            events.streams.push(
                zbus::block_on(MessageStream::for_match_rule(
                    rule,
                    connection.inner(),
                    Some(64),
                ))
                .map_err(|error| error.to_string())?,
            );
        }
        Ok(events)
    }

    pub fn drain(&mut self, active: &str) -> Result<(), WifiError> {
        let mut messages = Vec::new();
        for stream in &mut self.streams {
            while let Some(Some(message)) = stream.next().now_or_never() {
                messages.push(message.map_err(|error| error.to_string())?);
            }
        }
        messages.sort_by_key(zbus::Message::recv_position);
        for message in messages {
            let header = message.header();
            let path = header.path().map(|path| path.as_str());
            let interface = header.interface().map(|interface| interface.as_str());
            if interface == Some("org.freedesktop.NetworkManager.Connection.Active")
                && path == Some(active)
            {
                let (state, reason): (u32, u32) = message
                    .body()
                    .deserialize()
                    .map_err(|error| error.to_string())?;
                if matches!(state, 1 | 2) {
                    self.owns_device = true;
                }
                if matches!(state, 3 | 4) && matches!(reason, 9 | 10) {
                    self.credentials_rejected = true;
                }
            } else if path == Some(self.device.as_str())
                && interface == Some("org.freedesktop.DBus.Properties")
            {
                let (interface, changed, _): (
                    String,
                    std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
                    Vec<String>,
                ) = message
                    .body()
                    .deserialize()
                    .map_err(|error| error.to_string())?;
                // Clearing the owner can precede the failure signal; only a
                // different owner ends attribution to this activation attempt.
                if interface == "org.freedesktop.NetworkManager.Device"
                    && let Some(owner) = changed.get("ActiveConnection")
                    && let Ok(owner) = <&zbus::zvariant::ObjectPath>::try_from(owner)
                    && owner.as_str() != "/"
                {
                    self.owns_device = owner.as_str() == active;
                }
            } else if path == Some(self.device.as_str())
                && interface == Some("org.freedesktop.NetworkManager.Device")
            {
                let (state, _, reason): (u32, u32, u32) = message
                    .body()
                    .deserialize()
                    .map_err(|error| error.to_string())?;
                if self.owns_device && state == 120 && reason == 7 {
                    self.credentials_rejected = true;
                }
            }
        }
        Ok(())
    }

    pub fn failure(&self) -> WifiError {
        if self.credentials_rejected {
            WifiError {
                message: "Could not connect. Check the network credentials and try again.".into(),
                retry_password: true,
            }
        } else {
            "Could not connect. Check the network and try again.".into()
        }
    }
}

impl Drop for Events {
    fn drop(&mut self) {
        // Match removal can spawn D-Bus work, including on an ordinary blocking worker.
        zbus::block_on(async {
            for stream in self.streams.drain(..) {
                stream.async_drop().await;
            }
        });
    }
}
