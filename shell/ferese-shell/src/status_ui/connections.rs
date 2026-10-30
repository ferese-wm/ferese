use cosmic::app::Task;
use cosmic::iced::window;

use super::{Menu, MenuRows, MenuStyle};
use crate::status::wireless::pairing::{self, Request};
use crate::status::wireless::{self, BluetoothDevice, Password, WifiNetwork};
use crate::status::{Action, Discovery};
use crate::{FereseShell, Message};
#[cfg(test)]
mod tests;

#[derive(Default)]
pub(crate) struct State {
    pub wifi: Option<WifiNetwork>,
    pub password: Password,
    pub error: Option<(Discovery, String)>,
    pub pending: Option<Pending>,
    pub pairing: Option<Request>,
    pub pairing_text: Password,
    pub return_to_system: bool,
    failed_wifi: Option<WifiNetwork>,
    settings: std::cell::OnceCell<[bool; 2]>,
    pairing_cancel: Option<cosmic::iced::futures::channel::oneshot::Sender<()>>,
    serial: u64,
    owner: Option<window::Id>,
    pairing_owner: Option<window::Id>,
}

pub(crate) struct Pending {
    pub serial: u64,
    pub radio: Discovery,
    pub name: String,
    pub path: String,
    pub disconnecting: bool,
    wifi: Option<WifiNetwork>,
    requires_pairing: bool,
    pub cancelled: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum Event {
    ShowPanel(Menu),
    Wifi(WifiNetwork),
    Bluetooth(BluetoothDevice),
    Password(Password),
    Submit,
    Cancel,
    Refresh,
    Finished(u64, Result<(), String>),
    WifiFinished(u64, Result<(), wireless::WifiError>),
    CancellationFailed(u64, String),
    OpenSettings(Discovery),
    SettingsFinished(Discovery, Result<(), String>),
    Pairing(u64, Option<Request>),
    PairingText(u64, u64, Password),
    PairingReply(u64, u64, bool),
}

impl FereseShell {
    pub(crate) fn clear_connection_input(&mut self, surface: window::Id) {
        let current = self.connection_menu();
        if self.connections.owner == Some(surface)
            || (self.connections.owner.is_none() && current == Some((surface, Discovery::Wifi)))
        {
            self.connections.wifi = None;
            self.connections.password = Password::default();
            self.connections.owner = None;
        }
        if self.connections.pairing_owner == Some(surface)
            || (self.connections.pairing_owner.is_none()
                && current == Some((surface, Discovery::Bluetooth)))
        {
            self.cancel_connection_pairing();
        }
        if current.is_some_and(|(owner, _)| owner == surface) {
            self.connections.return_to_system = false;
        }
    }

    fn cancel_connection_pairing(&mut self) {
        self.connections.pairing_owner = None;
        self.connections.pairing = None;
        self.connections.pairing_text = Password::default();
        if let Some(pending) = &mut self.connections.pending
            && pending.requires_pairing
        {
            pending.cancelled = true;
            if let Some(cancel) = self.connections.pairing_cancel.take() {
                let _ = cancel.send(());
            }
        }
    }
    pub(super) fn connection_menu(&self) -> Option<(window::Id, Discovery)> {
        let radio = |kind| match kind {
            Menu::Network
                if self
                    .status
                    .network
                    .as_ref()
                    .is_some_and(|status| status.enabled) =>
            {
                Some(Discovery::Wifi)
            }
            Menu::Bluetooth
                if self
                    .status
                    .bluetooth
                    .as_ref()
                    .is_some_and(|status| status.enabled) =>
            {
                Some(Discovery::Bluetooth)
            }
            _ => None,
        };
        if let Some(menu) = &self.menu
            && !menu.motion.closing()
            && let Some(radio) = radio(menu.kind)
        {
            return Some((menu.id, radio));
        }
        self.outputs
            .iter()
            .filter(|output| !output.hidden)
            .find_map(|output| {
                let hub = output.runtime.hub.as_ref()?.borrow();
                if !matches!(hub.hub.mode(), ferese_panel::morph::Mode::Expanded(_)) {
                    return None;
                }
                Some((output.bar, radio(hub.menu?)?))
            })
    }

    pub(crate) fn sync_connection_discovery(&mut self) {
        let menu = self.connection_menu();
        let discovery = menu.map_or(Discovery::None, |(_, radio)| radio);
        if self
            .connections
            .pairing_owner
            .is_some_and(|owner| menu != Some((owner, Discovery::Bluetooth)))
        {
            self.cancel_connection_pairing();
        }
        if self
            .connections
            .owner
            .is_some_and(|owner| menu != Some((owner, Discovery::Wifi)))
        {
            self.connections.wifi = None;
            self.connections.password = Password::default();
            self.connections.owner = None;
        }
        if let Err(error) = self.status_service.set_discovery(discovery) {
            self.status_error = Some(error);
        }
    }

    pub(crate) fn update_connections(&mut self, event: Event) -> Task<Message> {
        match event {
            Event::ShowPanel(kind)
                if matches!(
                    kind,
                    Menu::System | Menu::Network | Menu::Bluetooth | Menu::Battery
                ) =>
            {
                let task = if let Some(menu) = &self.menu {
                    self.open_menu(kind, menu.anchor.clone())
                } else if let Some(id) = self.outputs.iter().find_map(|output| {
                    output.runtime.hub.as_ref().and_then(|hub| {
                        matches!(
                            hub.borrow().hub.mode(),
                            ferese_panel::morph::Mode::Expanded(_)
                        )
                        .then_some(output.bar)
                    })
                }) {
                    self.open_hub(id, kind)
                } else {
                    return Task::none();
                };
                self.connections.return_to_system = kind != Menu::System;
                return task;
            }
            Event::Pairing(serial, request) => {
                if !self
                    .connections
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.serial == serial && !pending.cancelled)
                {
                    if let Some(request) = request {
                        request.answer(None);
                    }
                    return Task::none();
                }
                if self
                    .connection_menu()
                    .is_none_or(|(_, radio)| radio != Discovery::Bluetooth)
                {
                    if let Some(request) = request {
                        request.answer(None);
                    }
                    self.cancel_connection_pairing();
                    return Task::none();
                }
                let needs_input = request.as_ref().is_some_and(|request| {
                    matches!(
                        request.prompt,
                        pairing::Prompt::Pin | pairing::Prompt::Passkey
                    )
                });
                if request.is_some() {
                    self.connections.pending.as_mut().unwrap().requires_pairing = true;
                }
                self.connections.pairing = request;
                self.connections.pairing_text = Password::default();
                if needs_input {
                    return cosmic::widget::text_input::focus(cosmic::widget::Id::new(
                        "bluetooth-pairing",
                    ));
                }
            }
            Event::PairingText(serial, request, text)
                if self
                    .connections
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.serial == serial)
                    && self
                        .connections
                        .pairing
                        .as_ref()
                        .is_some_and(|current| current.id == request) =>
            {
                self.connections.pairing_text = text;
            }
            Event::PairingReply(serial, request, accepted) => {
                if self
                    .connections
                    .pending
                    .as_ref()
                    .is_none_or(|pending| pending.serial != serial)
                    || self
                        .connections
                        .pairing
                        .as_ref()
                        .is_none_or(|current| current.id != request)
                {
                    return Task::none();
                }
                if accepted
                    && !self
                        .connections
                        .pairing
                        .as_ref()
                        .unwrap()
                        .accepts(self.connections.pairing_text.as_str())
                {
                    return Task::none();
                }
                let request = self.connections.pairing.take().unwrap();
                if accepted {
                    request.answer(Some(std::mem::take(&mut self.connections.pairing_text)));
                } else {
                    request.answer(None);
                    self.cancel_connection_pairing();
                }
            }
            Event::OpenSettings(radio) => {
                return Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || wireless::open_settings(radio))
                            .await
                            .unwrap_or_else(|_| Err("Settings could not be opened".into()))
                    },
                    move |result| {
                        cosmic::Action::App(Message::Connections(Event::SettingsFinished(
                            radio, result,
                        )))
                    },
                );
            }
            Event::SettingsFinished(radio, Err(error)) => {
                self.connections.error = Some((radio, error));
            }
            Event::Password(password) if self.connections.wifi.is_some() => {
                self.connections.password = password;
            }
            Event::Cancel => {
                self.connections.wifi = None;
                self.connections.password = Password::default();
                self.connections.owner = None;
            }
            Event::Refresh => {
                self.connections.error = None;
                if let Some((_, kind)) = self.connection_menu()
                    && let Err(error) = self.status_service.send(Action::Discover(kind))
                {
                    self.status_error = Some(error);
                }
            }
            Event::Wifi(network) if self.connections.pending.is_none() => {
                if self
                    .connection_menu()
                    .is_none_or(|(_, radio)| radio != Discovery::Wifi)
                {
                    return Task::none();
                }
                self.connections.error = None;
                let retry = self.connections.failed_wifi.as_ref().is_some_and(|failed| {
                    failed.ssid == network.ssid && failed.device == network.device
                });
                if network.security.needs_password()
                    && (network.saved.is_none() || retry)
                    && !network.connected
                {
                    self.connections.wifi = Some(network);
                    self.connections.password = Password::default();
                    self.connections.owner = self.connection_menu().map(|(id, _)| id);
                    return cosmic::widget::text_input::focus(cosmic::widget::Id::new(
                        "wifi-password",
                    ));
                } else {
                    return self.connect_wifi(network);
                }
            }
            Event::Submit if self.connections.pending.is_none() => {
                if self.connections.password.as_str().is_empty() {
                    return Task::none();
                }
                if let Some(network) = self.connections.wifi.take() {
                    return self.connect_wifi(network);
                }
            }
            Event::Bluetooth(device) if self.connections.pending.is_none() => {
                if self
                    .connection_menu()
                    .is_none_or(|(_, radio)| radio != Discovery::Bluetooth)
                {
                    return Task::none();
                }
                let serial = self.begin_connection(
                    Discovery::Bluetooth,
                    device.name.clone(),
                    device.path.clone(),
                );
                self.connections.pending.as_mut().unwrap().requires_pairing =
                    !device.paired && !device.connected;
                self.connections.pairing_owner = self.connection_menu().map(|(owner, _)| owner);
                self.connections.pending.as_mut().unwrap().disconnecting = device.connected;
                let (cancel, cancelled) = cosmic::iced::futures::channel::oneshot::channel();
                self.connections.pairing_cancel = Some(cancel);
                return Task::run(pairing::connect(device, cancelled), move |update| {
                    cosmic::Action::App(Message::Connections(match update {
                        pairing::Update::Prompt(request) => Event::Pairing(serial, Some(request)),
                        pairing::Update::Clear => Event::Pairing(serial, None),
                        pairing::Update::Finished(result) => Event::Finished(serial, result),
                        pairing::Update::CancellationFailed(error) => {
                            Event::CancellationFailed(serial, error)
                        }
                    }))
                });
            }
            Event::CancellationFailed(serial, error) => {
                if let Some(pending) = &mut self.connections.pending
                    && pending.serial == serial
                {
                    pending.cancelled = false;
                }
                return self.update_connections(Event::Finished(serial, Err(error)));
            }
            Event::WifiFinished(serial, result) => {
                if let Err(error) = &result
                    && !error.retry_password
                    && let Some(pending) = &mut self.connections.pending
                    && pending.serial == serial
                {
                    // Incompatible profiles and transport failures cannot be fixed by retyping a password.
                    pending.wifi = None;
                    self.connections.failed_wifi = None;
                }
                return self.update_connections(Event::Finished(
                    serial,
                    result.map_err(|error| error.to_string()),
                ));
            }
            Event::Finished(serial, result) => {
                if self
                    .connections
                    .pending
                    .as_ref()
                    .is_none_or(|pending| pending.serial != serial)
                {
                    return Task::none();
                }
                let pending = self.connections.pending.take().unwrap();
                self.connections.pairing = None;
                self.connections.pairing_text = Password::default();
                self.connections.pairing_cancel = None;
                self.connections.pairing_owner = None;
                if let Err(error) = result
                    && !pending.cancelled
                {
                    if self.connection_menu().is_none() {
                        self.notifications.service_error(&pending.name, &error);
                    }
                    self.connections.error = Some((pending.radio, error));
                    if let Some(network) = pending.wifi
                        && !network.connected
                        && network.security.needs_password()
                    {
                        self.connections.failed_wifi = Some(network.clone());
                        if let Some((owner, Discovery::Wifi)) = self.connection_menu() {
                            let current = self
                                .status
                                .network
                                .as_ref()
                                .and_then(|status| status.available.as_ref())
                                .and_then(|networks| {
                                    networks.iter().find(|current| {
                                        current.ssid == network.ssid
                                            && current.device == network.device
                                            && !current.connected
                                    })
                                })
                                .cloned();
                            if let Some(current) = current {
                                self.connections.wifi = Some(current);
                                self.connections.password = Password::default();
                                self.connections.owner = Some(owner);
                            }
                        }
                    }
                } else if pending.radio == Discovery::Wifi {
                    self.connections.failed_wifi = None;
                }
                if let Err(error) = self.status_service.send(Action::Refresh) {
                    self.status_error = Some(error);
                }
            }
            _ => {}
        }
        Task::none()
    }

    fn begin_connection(&mut self, radio: Discovery, name: String, path: String) -> u64 {
        self.connections.serial = self.connections.serial.wrapping_add(1);
        let serial = self.connections.serial;
        self.connections.error = None;
        self.connections.pending = Some(Pending {
            serial,
            radio,
            name,
            path,
            disconnecting: false,
            wifi: None,
            requires_pairing: false,
            cancelled: false,
        });
        serial
    }

    fn connect_wifi(&mut self, network: WifiNetwork) -> Task<Message> {
        let serial =
            self.begin_connection(Discovery::Wifi, network.name.clone(), network.path.clone());
        self.connections.pending.as_mut().unwrap().wifi = Some(network.clone());
        self.connections.pending.as_mut().unwrap().disconnecting = network.connected;
        let password = std::mem::take(&mut self.connections.password);
        self.connections.wifi = None;
        self.connections.owner = None;
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || wireless::connect_wifi(&network, &password))
                    .await
                    .unwrap_or_else(|_| Err("Wi-Fi connection did not finish".into()))
            },
            move |result| {
                cosmic::Action::App(Message::Connections(Event::WifiFinished(serial, result)))
            },
        )
    }
}

pub(super) fn heading<'a>(
    shell: &'a FereseShell,
    kind: Menu,
    style: MenuStyle,
) -> super::MenuHeading<'a> {
    use cosmic::iced::{Alignment, Length};
    use cosmic::widget::{button, row};
    let mut heading = row::with_capacity(2)
        .height(28)
        .spacing(6)
        .align_y(Alignment::Center);
    if shell.connections.return_to_system {
        heading = heading.push(
            button::custom(crate::bar_icon(
                ferese_theme::icons::CHEVRON_LEFT,
                14,
                style.primary,
            ))
            .padding([6, 4])
            .name("Back to Control Center")
            .on_press(cosmic::Action::App(Message::Connections(Event::ShowPanel(
                Menu::System,
            )))),
        );
    }
    let title = if kind == Menu::Network {
        "Wi-Fi"
    } else {
        "Bluetooth"
    };
    heading.push(
        shell
            .render
            .text(title)
            .size(14)
            .class(cosmic::theme::Text::Color(style.primary))
            .width(Length::Fill),
    )
}

pub(super) fn connection_row<'a>(
    shell: &FereseShell,
    source: &'static [u8],
    title: &'a str,
    detail: &'a str,
    action: Option<Message>,
    selected: bool,
    style: MenuStyle,
) -> cosmic::Element<'a, cosmic::Action<Message>> {
    use cosmic::iced::{Alignment, Length};
    use cosmic::widget::{button, column, container, row};
    let render = shell.render;
    let enabled = action.is_some() && shell.connections.pending.is_none();
    let label = |text: &'a str, size: u16, color| {
        render
            .text(text)
            .size(size)
            .line_height(cosmic::iced::widget::text::LineHeight::Absolute(
                (f32::from(size) + 1.).into(),
            ))
            .width(Length::Fill)
            .wrapping(cosmic::iced::widget::text::Wrapping::None)
            .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
            ))
            .class(cosmic::theme::Text::Color(color))
    };
    let tint = if selected {
        crate::color_with_opacity(style.theme.accent, style.opacity)
    } else {
        style.primary
    };
    let mut content = row![
        container(crate::bar_icon(source, 16, tint)).center_x(20),
        column![
            label(title, 13, style.primary),
            label(detail, 11, style.muted)
        ]
        .spacing(1)
        .width(Length::Fill),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if selected {
        content = content.push(crate::bar_icon(ferese_theme::icons::CHECK, 14, tint));
    }
    let mut button = button::custom(content)
        .id(match action.as_ref() {
            Some(Message::Connections(Event::Bluetooth(device))) => {
                cosmic::widget::Id::new(format!(
                    "bluetooth:{}:{}:{}:{enabled}",
                    device.path, device.connected, device.paired
                ))
            }
            _ => cosmic::widget::Id::new(format!("connection:readonly:{title}:{detail}")),
        })
        .width(Length::Fill)
        .padding([7, 8])
        .name(format!("{title}: {detail}"));
    if let Some(action) = action
        && enabled
    {
        button = button.on_press(cosmic::Action::App(action));
    }
    // The inner ID resets pressed state when a different target occupies this row.
    crate::motion::button(render, button, tint, selected, style.opacity)
}

pub(super) fn connection_list<'a>(
    rows: cosmic::Element<'a, cosmic::Action<Message>>,
    id: &'static str,
    height: f32,
) -> cosmic::Element<'a, cosmic::Action<Message>> {
    use cosmic::iced::Length;
    use cosmic::iced::widget::scrollable::{Direction, Scrollbar};
    cosmic::widget::container(
        cosmic::widget::scrollable(rows)
            .id(cosmic::widget::Id::new(id))
            .direction(Direction::Vertical(Scrollbar::hidden()))
            .height(Length::Shrink)
            .width(Length::Fill),
    )
    .max_height(height)
    .into()
}

pub(super) fn footer<'a>(
    shell: &'a FereseShell,
    mut rows: MenuRows<'a>,
    style: MenuStyle,
    radio: Discovery,
) -> MenuRows<'a> {
    use cosmic::iced::Length;
    use cosmic::widget::{Space, button, row, tooltip};
    if let Some((kind, error)) = &shell.connections.error
        && *kind == radio
    {
        rows = rows.push(
            shell
                .render
                .text(error)
                .size(12)
                .class(cosmic::theme::Text::Color(style.muted)),
        );
    }
    let icon_button = |source, label: &'static str, event| {
        let button = button::custom(crate::bar_icon(source, 14, style.primary))
            .id(cosmic::widget::Id::new(label))
            .padding(7)
            .name(label)
            .on_press(cosmic::Action::App(Message::Connections(event)));
        tooltip(
            crate::motion::button(shell.render, button, style.primary, false, style.opacity),
            shell.render.text(label).size(11),
            tooltip::Position::Top,
        )
    };
    let mut footer = row![
        Space::new().width(Length::Fill),
        icon_button(ferese_theme::icons::RESTART, "Refresh", Event::Refresh),
    ]
    .spacing(2);
    let available = shell.connections.settings.get_or_init(|| {
        [
            crate::status::available("nm-connection-editor"),
            crate::status::available("blueman-manager"),
        ]
    });
    if available[usize::from(radio == Discovery::Bluetooth)] {
        footer = footer.push(icon_button(
            ferese_theme::icons::SETTINGS,
            if radio == Discovery::Wifi {
                "Wi-Fi settings"
            } else {
                "Bluetooth settings"
            },
            Event::OpenSettings(radio),
        ));
    }
    rows.push(footer.width(Length::Fill))
}

#[cfg(test)]
#[path = "connections_edge_cases.rs"]
mod edge_cases;
