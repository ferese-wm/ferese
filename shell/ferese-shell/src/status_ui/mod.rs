//! Status popup lifecycle, shared frame, and content dispatch.
mod audio;
mod bar;
mod battery;
mod bluetooth;
mod calendar;
mod controls;
mod lifecycle;
mod media;
mod network;
mod system;

use bar::status_icon;
use controls::{control_card, menu_button, toggle_row};
use cosmic::iced::Rectangle;
use cosmic::iced::border::Shape as BorderShape;
use cosmic::widget::column;
use status::{Action, Snapshot};

use super::*;

type MenuHeading<'a> = cosmic::widget::Row<'a, cosmic::Action<Message>, cosmic::Theme, cosmic::Renderer>;

type MenuRows<'a> = cosmic::widget::Column<'a, cosmic::Action<Message>, cosmic::Theme, cosmic::Renderer>;

#[derive(Clone, Copy)]
struct MenuStyle {
    theme: ShellTheme,
    primary: Color,
    muted: Color,
    opacity: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Menu {
    Network,
    Bluetooth,
    Audio,
    Battery,
    Calendar,
    Recording,
    Media,
    Notifications,
    System,
    Overflow,
}

impl Menu {
    fn action_error(self, error: &status::ActionError) -> Option<&str> {
        let relevant = self == Self::System
            || matches!(
                (self, &error.action),
                (Self::Network, Action::Wifi(_))
                    | (Self::Bluetooth, Action::Bluetooth(_))
                    | (Self::Audio, Action::Volume(_) | Action::Mute(_))
                    | (Self::Battery, Action::Brightness(_) | Action::PowerProfile(_))
                    | (Self::Notifications, Action::Dnd(_) | Action::Notifications)
            );
        relevant.then_some(error.message.as_str())
    }

    fn width(self) -> f32 {
        match self {
            Self::Overflow => 240.0,
            Self::System => 360.0,
            Self::Battery => 328.0,
            Self::Calendar => 268.0,
            Self::Notifications | Self::Media => 368.0,
            _ => 300.0,
        }
    }

    fn height_limit(self) -> f32 {
        if self == Self::Calendar { 270.0 } else { 720.0 }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Overflow => "More controls",
            Self::System => "Control Center",
            Self::Network => "Wi-Fi",
            Self::Bluetooth => "Bluetooth",
            Self::Audio => "Sound",
            Self::Battery => "Battery",
            Self::Calendar => "Calendar",
            Self::Recording => "Screen recording",
            Self::Media => "Now playing",
            Self::Notifications => "Notifications",
        }
    }

    fn available(self, status: &Snapshot) -> bool {
        match self {
            Self::Network => status.network.is_some(),
            Self::Bluetooth => status.bluetooth.is_some(),
            Self::Audio => status.audio.is_some(),
            Self::Battery => status.battery.is_some(),
            Self::Calendar | Self::Recording | Self::Media => true,
            Self::Notifications => status.notifications.is_some(),
            Self::System | Self::Overflow => true,
        }
    }
}

pub struct OpenMenu {
    pub anchor: Rectangle<i32>,
    pub id: window::Id,
    pub kind: Menu,
    pub motion: super::motion::PopupMotion,
    pub effects: Option<EffectsBinding>,
    pub regions: super::motion::Regions,
}

impl OpenMenu {
    pub(super) fn prepare_surface(&mut self, surface: &wl_surface::WlSurface) {
        if self.effects.is_none() {
            match EffectsBinding::attach_role(surface, None, 0.0) {
                Ok(binding) => self.effects = Some(binding),
                Err(error) => eprintln!("ferese-shell: popover material unavailable: {error}"),
            }
        }
        if let Some(effects) = &self.effects {
            let regions = self.regions.lock().unwrap();
            if let Err(error) = effects.set_regions(&regions) {
                eprintln!("ferese-shell: could not prepare popover material: {error}");
            }
        }
        self.motion.begin(Instant::now());
        super::EFFECT_FRAME_PENDING.store(false, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub fn progress(&self) -> f32 {
        self.motion.progress()
    }

    pub fn animating(&self) -> bool {
        self.motion.animating()
    }
}

impl FereseShell {
    fn view_status_menu(&self) -> Element<'_, cosmic::Action<Message>> {
        let Some(menu) = &self.menu else { return text("").into() };
        motion::frame_driven(
            menu.motion.revision(),
            |now| self.view_status_menu_at(now),
            |now| menu.motion.frame_active(now),
            |now| {
                if let Some(effects) = &menu.effects {
                    let _ = effects.set_presentation(
                        &menu.regions.lock().unwrap(),
                        menu.motion.progress_at(now),
                        [],
                        super::ferese_surface_effects_v1::Role::Popover,
                    );
                }
            },
            |now| {
                (menu.motion.closing() && !menu.motion.animating_at(now))
                    .then_some(cosmic::Action::App(Message::AnimateMenu))
            },
        )
    }

    fn view_status_menu_at(&self, now: Instant) -> Element<'_, cosmic::Action<Message>> {
        let Some(menu) = &self.menu else {
            return text("").into();
        };
        if menu.kind == Menu::Notifications && self.notifications.ready {
            return cosmic::widget::autosize::autosize(
                self.view_notifications_at(now),
                cosmic::iced::advanced::widget::Id::new("ferese-notification-center"),
            )
            .limits(
                Limits::NONE
                    .min_width(menu.kind.width())
                    .max_width(menu.kind.width())
                    .max_height(720.0),
            )
            .into();
        }
        let theme = self.config.theme;
        let p = 1.0;
        let kind = menu.kind;
        let primary = color_with_opacity(theme.text_primary, p);
        let muted = color_with_opacity(theme.text_muted, p);
        let badge = ferese_theme::menus::badge(
            accented_icon(
                status_icon(kind, &self.status).0,
                22,
                primary,
                color_with_opacity(theme.accent, p),
            )
            .width(Length::Fixed(22.))
            .into(),
            color(theme.accent),
            p,
            motion::radius(20.),
        );
        let mut heading = ferese_theme::menus::heading(badge, kind.title(), shell_font());
        let style = MenuStyle {
            theme,
            primary,
            muted,
            opacity: p,
        };
        heading = match kind {
            Menu::System => system::heading(self, heading, style),
            Menu::Network => network::heading(self, heading, style),
            Menu::Audio => audio::heading(self, heading, style),
            _ => heading,
        };
        let mut rows = column::with_capacity(12).spacing(12).width(Length::Fill);
        if kind != Menu::Calendar {
            rows = rows.push(heading);
        }
        rows = match kind {
            Menu::Overflow => self.view_status_overflow(rows, style),
            Menu::System => system::view(self, rows, style),
            Menu::Network => network::view(self, rows, style),
            Menu::Bluetooth => bluetooth::view(self, rows, style),
            Menu::Audio => audio::view(self, rows, style, false),
            Menu::Battery => battery::view(self, rows, style),
            Menu::Calendar => rows.push(calendar::view(self.calendar_offset, theme, p)),
            Menu::Notifications => notification_controls(self, rows, style, false),
            Menu::Recording => rows,
            Menu::Media => media::view(self, rows, style),
        };
        if kind != Menu::Calendar && !kind.available(&self.status) {
            rows = rows.push(text("Service unavailable").size(13).class(theme::Text::Color(muted)));
        }

        if let Some(error) = self
            .status_error
            .as_ref()
            .and_then(|error| menu.kind.action_error(error))
        {
            rows = rows.push(text(error).size(12).class(theme::Text::Color(Color {
                a: p,
                ..Color::from_rgb8(230, 172, 90)
            })));
        }

        let compositor_material = menu.effects.is_some();
        let panel = container(rows)
            .id("ferese-blur-card")
            .width(kind.width())
            .padding(16)
            .class(theme::Container::custom(move |_| container::Style {
                background: (!compositor_material)
                    .then_some(Background::Color(color_with_opacity(theme.surface_popover, p))),
                text_color: Some(primary),
                icon_color: Some(primary),
                border: Border {
                    shape: BorderShape::Continuous,
                    color: color_with_opacity(theme.border, p),
                    width: 1.0,
                    radius: theme.material_radius.into(),
                    ..Default::default()
                },
                snap: true,
                ..Default::default()
            }));

        cosmic::widget::autosize::autosize(
            super::motion::animated(
                panel.into(),
                menu.motion.progress_at(now),
                menu.regions.clone(),
                theme.material_radius,
            ),
            cosmic::iced::advanced::widget::Id::new("ferese-status-menu"),
        )
        .limits(
            Limits::NONE
                .min_width(kind.width())
                .max_width(kind.width())
                .max_height(kind.height_limit()),
        )
        .into()
    }
}

fn notification_controls<'a>(
    shell: &'a FereseShell,
    mut rows: MenuRows<'a>,
    style: MenuStyle,
    combined: bool,
) -> MenuRows<'a> {
    let MenuStyle {
        theme,
        primary,
        muted,
        opacity: p,
    } = style;
    if let Some(n) = &shell.status.notifications {
        let toggle = toggle_row("Do Not Disturb", n.dnd, Action::Dnd(!n.dnd), theme, p);
        rows = rows.push(if combined {
            control_card(toggle, primary, p)
        } else {
            toggle
        });

        if !combined {
            rows = rows
                .push(
                    text(if n.count == 0 {
                        "No notifications".to_owned()
                    } else if n.count == 1 {
                        "1 notification".to_owned()
                    } else {
                        format!("{} notifications", n.count)
                    })
                    .size(13)
                    .class(theme::Text::Color(muted)),
                )
                .push(menu_button(
                    "Open notification center",
                    Message::Control(Action::Notifications),
                    primary,
                    p,
                ));
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_controls_only_show_errors_in_related_popups() {
        for (action, relevant) in [
            (Action::Bluetooth(true), Menu::Bluetooth),
            (Action::Wifi(true), Menu::Network),
            (Action::Volume(50), Menu::Audio),
            (Action::Mute(true), Menu::Audio),
            (Action::Brightness(50), Menu::Battery),
            (Action::PowerProfile("balanced"), Menu::Battery),
            (Action::Dnd(true), Menu::Notifications),
        ] {
            let error = status::ActionError {
                action,
                message: "control failed".into(),
            };
            for menu in [
                Menu::Network,
                Menu::Bluetooth,
                Menu::Audio,
                Menu::Battery,
                Menu::Calendar,
                Menu::Recording,
                Menu::Notifications,
                Menu::System,
            ] {
                assert_eq!(
                    menu.action_error(&error),
                    (menu == relevant || menu == Menu::System).then_some("control failed"),
                    "{menu:?} showed the wrong control error: {:?}",
                    error.action
                );
            }
        }
    }

    #[test]
    fn smaller_popups_have_content_specific_widths() {
        assert_eq!(Menu::System.width(), 360.0);
        assert_eq!(Menu::Network.width(), 300.0);
        assert_eq!(Menu::Bluetooth.width(), 300.0);
        assert_eq!(Menu::Battery.width(), 328.0);
        assert_eq!(Menu::Calendar.width(), 268.0);
        assert_eq!(Menu::Calendar.height_limit(), 270.0);
        assert_eq!(Menu::Audio.width(), 300.0);
        assert_eq!(Menu::Notifications.width(), 368.0);
    }

    #[test]
    fn completed_open_stops_requesting_animation_ticks() {
        let past = Instant::now() - Duration::from_secs(1);
        let mut motion = super::motion::PopupMotion::new(Default::default());
        motion.begin(past);
        let menu = OpenMenu {
            anchor: Rectangle::default(),
            id: window::Id::unique(),
            kind: Menu::Network,
            motion,
            effects: None,
            regions: Default::default(),
        };
        assert_eq!(menu.progress(), 1.0);
        assert!(!menu.animating());
    }

    #[test]
    fn services_are_hidden_until_available() {
        let s = Snapshot::default();
        assert!(!Menu::Network.available(&s));
        assert!(!Menu::Notifications.available(&s));
        assert!(Menu::System.available(&s));
    }
}

impl From<ferese_config::status::StatusItem> for Menu {
    fn from(item: ferese_config::status::StatusItem) -> Self {
        use ferese_config::status::StatusItem;
        match item {
            StatusItem::Media => Self::Media,
            StatusItem::System => Self::System,
            StatusItem::Network => Self::Network,
            StatusItem::Bluetooth => Self::Bluetooth,
            StatusItem::Audio => Self::Audio,
            StatusItem::Recording => Self::Recording,
            StatusItem::Notifications => Self::Notifications,
            StatusItem::Battery => Self::Battery,
        }
    }
}

impl FereseShell {
    fn status_items(
        &self,
    ) -> (
        Vec<ferese_config::status::StatusItem>,
        Vec<ferese_config::status::StatusItem>,
    ) {
        self.config.status.icons.partition(|item| {
            Menu::from(item).available(&self.status)
                && (item != ferese_config::status::StatusItem::Media || self.media.snapshot.selected.is_some())
        })
    }

    fn view_status_overflow<'a>(&'a self, mut rows: MenuRows<'a>, style: MenuStyle) -> MenuRows<'a> {
        let (_, hidden) = self.status_items();
        for item in hidden {
            let label = if item == ferese_config::status::StatusItem::Recording && self.recorder.busy() {
                self.recorder.label()
            } else {
                item.label()
            };
            let content = row![
                accented_icon(
                    status_icon(item.into(), &self.status).0,
                    16,
                    style.primary,
                    color(style.theme.accent)
                ),
                text(label).size(13),
            ]
            .spacing(8)
            .align_y(cosmic::iced::Alignment::Center);
            rows = rows.push(motion::button(
                button::custom(content)
                    .name(label)
                    .width(Length::Fill)
                    .padding([6, 8])
                    .on_press(cosmic::Action::App(Message::ActivateStatusItem(item))),
                style.primary,
                false,
                style.opacity,
            ));
        }
        rows.spacing(4)
    }

    pub(crate) fn activate_status_item(&mut self, item: ferese_config::status::StatusItem) -> Task<Message> {
        let Some(menu) = &self.menu else {
            return Task::none();
        };
        // Popup row coordinates are not bar coordinates. Keep the original bar anchor.
        let anchor = menu.anchor;
        if menu.kind != Menu::Overflow || menu.motion.closing() || !self.status_items().1.contains(&item) {
            return Task::none();
        }
        let message = status_item_action(item.into(), self.recorder.busy(), anchor);
        match message {
            Message::OpenMenu(kind, anchor) => self.open_menu(kind, anchor),
            action => self
                .close_menu()
                .chain(cosmic::task::message(cosmic::Action::App(action))),
        }
    }
}

fn status_item_action(kind: Menu, recording_busy: bool, anchor: Rectangle<i32>) -> Message {
    if kind == Menu::Recording {
        if recording_busy {
            Message::StopRecording
        } else {
            Message::StartRecording
        }
    } else {
        Message::OpenMenu(kind, anchor)
    }
}

#[cfg(test)]
mod overflow_tests {
    use super::*;
    use ferese_config::status::{StatusItem, StatusVisibility};

    #[test]
    fn hidden_items_dispatch_existing_actions_with_the_bar_anchor() {
        let anchor = Rectangle {
            x: 800,
            y: 0,
            width: 24,
            height: 28,
        };
        for item in StatusItem::ALL {
            for busy in [false, true] {
                match status_item_action(item.into(), busy, anchor) {
                    Message::StartRecording => assert_eq!((item, busy), (StatusItem::Recording, false)),
                    Message::StopRecording => assert_eq!((item, busy), (StatusItem::Recording, true)),
                    Message::OpenMenu(kind, actual) => {
                        assert_ne!(item, StatusItem::Recording);
                        assert_eq!(kind, Menu::from(item));
                        assert_eq!(actual, anchor);
                    }
                    _ => panic!("status item lost its action"),
                }
            }
        }
    }

    #[test]
    fn service_availability_applies_to_both_groups() {
        let status = Snapshot::default();
        let (visible, hidden) = StatusVisibility::default().partition(|item| Menu::from(item).available(&status));
        assert_eq!(
            visible,
            vec![StatusItem::Media, StatusItem::System, StatusItem::Recording]
        );
        assert!(hidden.is_empty());
    }
}
