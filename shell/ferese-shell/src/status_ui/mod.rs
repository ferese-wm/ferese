//! Status popup lifecycle, shared frame, and content dispatch.
mod audio;
mod bar;
mod battery;
mod bluetooth;
mod calendar;
mod controls;
mod lifecycle;
mod network;
mod system;

use bar::status_icon;
use controls::{control_card, menu_button, toggle_row};
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
    Notifications,
    System,
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
            Self::System => 360.0,
            Self::Battery => 328.0,
            Self::Calendar => 268.0,
            Self::Notifications => 368.0,
            _ => 300.0,
        }
    }

    fn height_limit(self) -> f32 {
        if self == Self::Calendar { 270.0 } else { 720.0 }
    }

    fn title(self) -> &'static str {
        match self {
            Self::System => "Control Center",
            Self::Network => "Wi-Fi",
            Self::Bluetooth => "Bluetooth",
            Self::Audio => "Sound",
            Self::Battery => "Battery",
            Self::Calendar => "Calendar",
            Self::Recording => "Screen recording",
            Self::Notifications => "Notifications",
        }
    }

    fn available(self, status: &Snapshot) -> bool {
        match self {
            Self::Network => status.network.is_some(),
            Self::Bluetooth => status.bluetooth.is_some(),
            Self::Audio => status.audio.is_some(),
            Self::Battery => status.battery.is_some(),
            Self::Calendar | Self::Recording => true,
            Self::Notifications => status.notifications.is_some(),
            Self::System => true,
        }
    }
}

pub struct OpenMenu {
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
            Menu::System => system::view(self, rows, style),
            Menu::Network => network::view(self, rows, style),
            Menu::Bluetooth => bluetooth::view(self, rows, style),
            Menu::Audio => audio::view(self, rows, style, false),
            Menu::Battery => battery::view(self, rows, style),
            Menu::Calendar => rows.push(calendar::view(self.calendar_offset, theme, p)),
            Menu::Notifications => notification_controls(self, rows, style, false),
            Menu::Recording => rows,
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
