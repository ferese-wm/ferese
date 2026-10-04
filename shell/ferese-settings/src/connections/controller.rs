use std::sync::Arc;

use cosmic::app::Task;
use zeroize::{Zeroize, Zeroizing};

use super::ui::Input;
use super::{Client, Command, Network, Snapshot};
use crate::{App, Message};

impl App {
    pub(crate) fn refresh_connections(&mut self) -> Task<Message> {
        if self.connections.loading {
            return Task::none();
        }
        self.connections.loading = true;
        if let Some(client) = self.connections.client.clone() {
            Task::perform(async move { Box::new(client.snapshot().await) }, |snapshot| {
                cosmic::Action::App(Message::ConnectionsRefreshed(snapshot))
            })
        } else {
            Task::perform(Client::new(), |client| {
                cosmic::Action::App(Message::ConnectionsReady(client))
            })
        }
    }

    pub(crate) fn connections_ready(&mut self, result: Result<Arc<Client>, String>) -> Task<Message> {
        self.connections.loading = false;
        match result {
            Ok(client) => {
                self.connections.client = Some(client);
                self.connections.error = None;
                self.refresh_connections()
            }
            Err(error) => {
                self.connections.error = Some(error);
                Task::none()
            }
        }
    }

    pub(crate) fn connections_refreshed(&mut self, snapshot: Snapshot) {
        self.connections.loading = false;
        if let Ok(bluetooth) = &snapshot.bluetooth
            && !bluetooth
                .adapters
                .iter()
                .any(|a| Some(&a.path) == self.connections.adapter.as_ref())
        {
            self.connections.adapter = bluetooth.adapters.first().map(|a| a.path.clone());
        }
        self.connections.snapshot = Some(snapshot);
    }

    pub(crate) fn connection_finished(&mut self, result: Result<(), String>) -> Task<Message> {
        self.connections.busy = None;
        self.connections.error = result.err();
        self.connections.prompt = None;
        self.connections.pairing_input.zeroize();
        self.refresh_connections()
    }

    pub(crate) fn leave_connections(&mut self) -> Task<Message> {
        self.connections.password.zeroize();
        self.connections.pairing_input.zeroize();
        self.connections.selected = None;
        self.connections.hidden = false;
        self.connections.prompt = None;
        self.connections.forget = None;
        if let Some(client) = &self.connections.client {
            client.leave_now();
        }
        Task::none()
    }

    pub(crate) fn connections_input(&mut self, input: Input) -> Task<Message> {
        match input {
            Input::Refresh => return self.refresh_connections(),
            Input::PollPrompt => {
                let prompt = self.connections.client.as_ref().and_then(|c| c.prompt());
                if prompt != self.connections.prompt {
                    self.connections.pairing_input.zeroize();
                    let needs_input = prompt
                        .as_ref()
                        .is_some_and(|p| matches!(p.kind, super::agent::Kind::Pin | super::agent::Kind::Passkey));
                    self.connections.prompt = prompt;
                    let scroll = cosmic::iced::widget::scrollable::snap_to(
                        cosmic::widget::Id::new("settings-content"),
                        cosmic::iced::widget::scrollable::RelativeOffset { x: None, y: Some(0.) },
                    );
                    return if needs_input {
                        Task::batch([
                            scroll,
                            cosmic::widget::text_input::focus(cosmic::widget::Id::new("bluetooth-passkey")),
                        ])
                    } else {
                        scroll
                    };
                }
            }
            Input::Tab(tab) => {
                self.connections.tab = tab;
                self.connections.password.zeroize();
                self.connections.selected = None;
                self.connections.hidden = false;
            }
            Input::Adapter(path) => self.connections.adapter = Some(path),
            Input::Password(value) => self.connections.password = value,
            Input::HiddenName(name) => self.connections.hidden_name = name,
            Input::HiddenSecurity(security) => self.connections.hidden_security = security,
            Input::Select(network) => {
                self.connections.password.zeroize();
                self.connections.hidden = false;
                self.connections.selected = Some(network);
                self.connections.error = None;
                return cosmic::widget::text_input::focus(cosmic::widget::Id::new("wifi-password"));
            }
            Input::Hidden(hidden) => {
                self.connections.hidden = hidden;
                self.connections.selected = None;
                self.connections.password.zeroize();
                self.connections.hidden_name.clear();
            }
            Input::CancelWifi => {
                self.connections.password.zeroize();
                self.connections.selected = None;
                self.connections.hidden = false;
            }
            Input::SubmitWifi => {
                if let Some(network) = self.wifi_target()
                    && self.connections.busy.is_none()
                    && network.security.can_create()
                    && super::wifi::valid_password(network.security, &self.connections.password)
                {
                    let password = std::mem::replace(&mut self.connections.password, Zeroizing::new(String::new()));
                    return self.connections_input(Input::Run(Command::WifiConnect(network, password)));
                }
            }
            Input::PairingValue(id, value) => {
                if self.connections.prompt.as_ref().is_some_and(|p| p.id == id) {
                    self.connections.pairing_input = value;
                }
            }
            Input::Answer(id, accepted) => {
                if self.connections.prompt.as_ref().is_none_or(|p| p.id != id) {
                    return Task::none();
                }
                if accepted
                    && !super::agent::valid_answer(
                        &self.connections.prompt.as_ref().unwrap().kind,
                        &self.connections.pairing_input,
                    )
                {
                    return Task::none();
                }
                if let Some(client) = &self.connections.client {
                    let value = accepted
                        .then(|| std::mem::replace(&mut self.connections.pairing_input, Zeroizing::new(String::new())));
                    client.reply(id, value);
                }
                self.connections.pairing_input.zeroize();
                self.connections.prompt = None;
            }
            Input::Forget(command) => {
                self.connections.forget = Some(command);
                return cosmic::iced::widget::scrollable::snap_to(
                    cosmic::widget::Id::new("settings-content"),
                    cosmic::iced::widget::scrollable::RelativeOffset { x: None, y: Some(0.) },
                );
            }
            Input::CancelForget => self.connections.forget = None,
            Input::ConfirmForget => {
                if let Some(command) = self.connections.forget.take() {
                    return self.connections_input(Input::Run(command));
                }
            }
            Input::Run(command) => {
                if self.connections.busy.is_some() && !matches!(command, Command::CancelPairing) {
                    return Task::none();
                }
                let Some(client) = self.connections.client.clone() else {
                    return Task::none();
                };
                if matches!(command, Command::CancelPairing) {
                    // The original Pair task still owns completion and busy state.
                    return Task::perform(
                        async move {
                            let _ = client.cancel_pairing().await;
                        },
                        |_| cosmic::Action::App(Message::ConnectionsLeft),
                    );
                }
                self.connections.busy = Some(command.label());
                self.connections.error = None;
                self.connections.selected = None;
                self.connections.hidden = false;
                let epoch = client.epoch();
                return Task::perform(async move { client.execute_checked(command, epoch).await }, |result| {
                    cosmic::Action::App(Message::ConnectionFinished(result))
                });
            }
        }
        Task::none()
    }

    pub(super) fn wifi_target(&self) -> Option<Network> {
        if self.connections.hidden {
            let wifi = self.connections.snapshot.as_ref()?.wifi.as_ref().ok()?;
            let radio = wifi.radios.first()?;
            let name = &self.connections.hidden_name;
            if name.is_empty() || name.len() > 32 {
                return None;
            }
            Some(Network {
                ssid: name.as_bytes().to_vec(),
                name: name.clone(),
                security: self.connections.hidden_security,
                strength: 0,
                device: radio.path.clone(),
                ap: "/".into(),
                saved: None,
                connected: false,
                connecting: false,
                hidden: true,
            })
        } else {
            self.connections.selected.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use cosmic::Application;

    use super::*;

    fn app() -> App {
        App::init(
            cosmic::app::Core::default(),
            (
                "/unused/connections-test.kdl".into(),
                crate::store::Snapshot::parse(String::new()),
                None,
            ),
        )
        .0
    }

    #[test]
    fn switching_tabs_and_leaving_clear_credentials_without_config_edits() {
        let mut app = app();
        let original = app.draft.source.clone();
        let _ = app.connections_input(Input::Password(Zeroizing::new("secret".into())));
        let _ = app.connections_input(Input::Tab(super::super::Tab::Bluetooth));
        assert!(app.connections.password.is_empty());
        let _ = app.connections_input(Input::Password(Zeroizing::new("another-secret".into())));
        let _ = app.leave_connections();
        assert!(app.connections.password.is_empty());
        assert!(app.connections.pairing_input.is_empty());
        assert!(app.pending.is_empty());
        assert_eq!(app.draft.source, original);
    }

    #[test]
    fn stale_pairing_input_and_submit_do_not_touch_the_current_question() {
        let mut app = app();
        app.connections.prompt = Some(super::super::agent::Prompt {
            id: 2,
            device: "/device/2".into(),
            kind: super::super::agent::Kind::Passkey,
        });
        app.connections.pairing_input = Zeroizing::new("123456".into());
        let _ = app.connections_input(Input::PairingValue(1, Zeroizing::new("old-value".into())));
        let _ = app.connections_input(Input::Answer(1, true));
        assert_eq!(app.connections.pairing_input.as_str(), "123456");
        assert_eq!(app.connections.prompt.as_ref().unwrap().id, 2);
        let _ = app.connections_input(Input::PairingValue(2, Zeroizing::new("bad-value".into())));
        let _ = app.connections_input(Input::Answer(2, true));
        assert!(app.connections.prompt.is_some());
    }

    #[test]
    fn debug_messages_redact_network_and_pairing_secrets() {
        let network = Network {
            ssid: b"test".to_vec(),
            name: "test".into(),
            security: super::super::Security::Personal,
            strength: 0,
            device: "/device".into(),
            ap: "/ap".into(),
            saved: None,
            connected: false,
            connecting: false,
            hidden: false,
        };
        for message in [
            Input::Run(Command::WifiConnect(network, Zeroizing::new("never-log-this".into()))),
            Input::Password(Zeroizing::new("never-log-this".into())),
            Input::PairingValue(1, Zeroizing::new("never-log-this".into())),
        ] {
            assert!(!format!("{message:?}").contains("never-log-this"));
        }
    }

    #[test]
    fn connection_page_search_and_startup_tab() {
        assert!(crate::schema::Page::Connections.matches("bluetooth"));
        assert!(crate::schema::Page::Connections.matches("wi-fi"));
        let (app, _) = App::init(
            cosmic::app::Core::default(),
            (
                "/unused/connections-test.kdl".into(),
                crate::store::Snapshot::parse(String::new()),
                Some(crate::InitialPage::Connections(super::super::Tab::Bluetooth)),
            ),
        );
        assert_eq!(app.page, crate::schema::Page::Connections);
        assert_eq!(app.connections.tab, super::super::Tab::Bluetooth);
        assert!(app.connections.loading);
    }
}
