use std::time::Instant;

use cosmic::app::Task;
use cosmic::iced::platform_specific::runtime::wayland::popup::{SctkPopupSettings, SctkPositioner};
use cosmic::iced::{Limits, Rectangle, window};
use wayland_client::Proxy;

use super::{Menu, OpenMenu};
use crate::status::Action;
use crate::{FereseShell, Message};

impl OpenMenu {
    fn switch_panel(&mut self, kind: Menu, settings: crate::motion::Settings, now: Instant) {
        self.kind = kind;
        // Keep the Wayland popup and its material binding, but give the new
        // panel its own entrance instead of reusing the settled old panel.
        self.motion = crate::motion::PopupMotion::new(settings);
        self.motion.begin(now);
    }
}

impl FereseShell {
    pub fn open_menu(&mut self, kind: Menu, anchor: Rectangle<i32>) -> Task<Message> {
        if self.menu.as_ref().is_some_and(|menu| menu.kind == kind) {
            if let Some(menu) = &mut self.menu
                && menu.motion.closing()
            {
                if kind == Menu::Media {
                    self.media.now_us = ferese_ipc::media::now_us();
                    self.media.refresh_art(true);
                }

                menu.motion.retarget(1.0, Instant::now());
                return Task::none();
            }

            return self.close_menu();
        }

        if kind == Menu::Media {
            self.media.now_us = ferese_ipc::media::now_us();
            self.media.refresh_art(true);
            self.media.error = None;
        }

        if kind != Menu::Media {
            self.refresh_media_art(false);
        }

        if kind == Menu::Calendar {
            self.calendar_offset = 0;
        }

        if !kind.available(&self.status) {
            return Task::none();
        }
        if self.menu.as_ref().is_some_and(|menu| menu.kind == Menu::Notifications) {
            self.notifications.history_open = false;
            self.notifications.hovered = None;
        }

        if kind == Menu::Notifications && self.notifications.ready {
            self.notifications.toggle_history();
        }

        let notifications = self.sync_notification_surface();
        crate::EFFECT_FRAME_PENDING.store(true, std::sync::atomic::Ordering::Relaxed);
        self.status_error = None;
        let parent = self.bar_surface_id;
        let anchor = if kind == Menu::System {
            self.outputs
                .iter()
                .find(|output| output.bar == parent)
                .and_then(|output| output.size)
                .map(|(width, _)| {
                    let theme = self.config.theme;
                    Rectangle {
                        x: (width - theme.bar_margin_horizontal * 2 - theme.panel_padding.round() as i32 - 1).max(0),
                        y: 0,
                        width: 1,
                        height: theme.bar_height.round() as i32,
                    }
                })
                .unwrap_or(anchor)
        } else {
            anchor
        };
        let height_limit = if kind == Menu::Notifications {
            self.notification_history_height_limit()
        } else {
            kind.height_limit()
        };
        let positioner = SctkPositioner {
            anchor_rect: anchor,
            anchor: 2u32.try_into().unwrap(),
            gravity: 6u32.try_into().unwrap(),
            offset: (anchor.width / 2, 8),
            // The view's autosize widget supplies the active panel's bounds.
            // Keep initial popup limits broad enough for later panel changes.
            size_limits: Limits::NONE.max_width(368.0).max_height(720.0_f32.max(height_limit)),
            constraint_adjustment: 3,
            ..Default::default()
        };
        if let Some(menu) = &mut self.menu {
            menu.switch_panel(kind, self.config.animations, Instant::now());
            return Task::batch([
                notifications,
                cosmic::iced::platform_specific::shell::commands::popup::reposition(menu.id, positioner),
            ]);
        }
        let id = window::Id::unique();
        self.menu = Some(OpenMenu {
            id,
            kind,
            motion: crate::motion::PopupMotion::new(self.config.animations),
            effects: None,
            regions: Default::default(),
        });
        let action = cosmic::surface::action::app_popup::<Self>(
            |_| Default::default(),
            move |_| SctkPopupSettings {
                id,
                parent,
                parent_size: None,
                grab: true,
                close_with_children: false,
                input_zone: None,
                positioner: positioner.clone(),
            },
            Some(Box::new(Self::view_status_menu)),
        );
        notifications.chain(cosmic::task::message(cosmic::Action::Surface(action)))
    }

    pub fn destroy_menu(&mut self) -> Task<Message> {
        crate::EFFECT_FRAME_PENDING.store(false, std::sync::atomic::Ordering::Relaxed);
        let Some(menu) = self.menu.take() else {
            return Task::none();
        };
        self.refresh_media_art(false);
        if menu.kind == Menu::Notifications {
            self.notifications.history_open = false;
            self.notifications.hovered = None;
        }
        Task::batch([
            cosmic::task::message(cosmic::Action::Surface(cosmic::surface::action::destroy_popup(menu.id))),
            self.sync_notification_surface(),
        ])
    }

    pub fn close_menu(&mut self) -> Task<Message> {
        self.refresh_media_art(false);
        let Some(menu) = &mut self.menu else {
            return Task::none();
        };
        if menu.motion.closing() {
            return Task::none();
        }
        if menu
            .effects
            .as_ref()
            .is_some_and(|effects| effects.surface.version() >= 3)
        {
            menu.motion.retarget(0.0, Instant::now());
            if menu.animating() {
                return Task::none();
            }
        }
        self.destroy_menu()
    }

    pub fn animate_menu(&mut self) -> Task<Message> {
        if let Some(menu) = &self.menu
            && menu.motion.closing()
            && !menu.animating()
        {
            return self.destroy_menu();
        }
        Task::none()
    }

    pub fn optimistic_status(&mut self, action: &Action) {
        match action {
            Action::Volume(v) => {
                if let Some(a) = &mut self.status.audio {
                    a.volume = *v;
                }
            }
            Action::Mute(v) => {
                if let Some(a) = &mut self.status.audio {
                    a.muted = *v;
                }
            }
            Action::Brightness(v) => self.status.brightness = Some(*v),
            Action::Wifi(v) => {
                if let Some(n) = &mut self.status.network {
                    n.enabled = *v;
                }
            }
            Action::Bluetooth(v) => {
                if let Some(b) = &mut self.status.bluetooth {
                    b.enabled = *v;
                }
            }
            Action::Dnd(v) => {
                if let Some(n) = &mut self.status.notifications {
                    n.dnd = *v;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::motion::{PopupMotion, Settings};
    use std::time::Duration;

    #[test]
    fn switching_panels_replays_entrance_on_the_same_surface() {
        let now = Instant::now();
        let settings = Settings::default();
        let mut menu = OpenMenu {
            id: window::Id::unique(),
            kind: Menu::Battery,
            motion: PopupMotion::new(settings),
            effects: None,
            regions: Default::default(),
        };
        let id = menu.id;
        let regions = menu.regions.clone();
        menu.motion.begin(now);
        for (index, kind) in [
            Menu::Network,
            Menu::Bluetooth,
            Menu::Audio,
            Menu::Calendar,
            Menu::Recording,
            Menu::Notifications,
            Menu::System,
            Menu::Battery,
        ]
        .into_iter()
        .enumerate()
        {
            let at = now + Duration::from_secs(10 * (index as u64 + 1));
            assert_eq!(menu.motion.progress_at(at), 1.0);
            menu.switch_panel(kind, settings, at);
            assert_eq!(menu.kind, kind);
            assert_eq!(menu.id, id);
            assert!(std::sync::Arc::ptr_eq(&regions, &menu.regions));
            assert_eq!(menu.motion.progress_at(at), 0.0);
            assert!(menu.motion.frame_active(at));
            let progress = menu.motion.progress_at(at + Duration::from_millis(30));
            assert!(progress > 0.0 && progress < 1.0);
        }
        let at = now + Duration::from_secs(90);
        menu.motion.retarget(0.0, at);
        menu.switch_panel(Menu::Network, settings, at + Duration::from_millis(30));
        assert!(!menu.motion.closing());
        assert!(menu.motion.frame_active(at + Duration::from_millis(30)));
    }

    #[test]
    fn panel_switch_respects_reduced_motion_and_speed() {
        let now = Instant::now();
        let mut menu = OpenMenu {
            id: window::Id::unique(),
            kind: Menu::Battery,
            motion: PopupMotion::new(Settings::default()),
            effects: None,
            regions: Default::default(),
        };
        for settings in [
            Settings {
                reduced_motion: true,
                ..Default::default()
            },
            Settings {
                enabled: false,
                ..Default::default()
            },
        ] {
            menu.switch_panel(Menu::Network, settings, now);
            assert_eq!(menu.motion.progress_at(now), 1.0);
            assert!(!menu.motion.frame_active(now));
        }
        menu.switch_panel(Menu::Battery, Settings::default(), now);
        let normal = menu.motion.progress_at(now + Duration::from_millis(30));
        menu.switch_panel(
            Menu::Network,
            Settings {
                speed: 0.5,
                ..Default::default()
            },
            now,
        );
        assert_eq!(menu.motion.progress_at(now + Duration::from_millis(60)), normal);
    }
}
