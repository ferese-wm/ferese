mod obs;
mod portal;

use std::sync::Arc;
use std::time::Duration;

use cosmic::iced::Subscription;
use ferese_ipc::capture::Session;

#[derive(Clone, Debug)]
pub(crate) enum Event {
    Sharing(Vec<Session>),
    Obs(obs::Snapshot),
}

#[derive(Default)]
pub(crate) struct Model {
    pub sessions: Vec<Session>,
    pub obs: obs::Snapshot,
    config: Option<Arc<obs::Config>>,
}

impl Model {
    pub fn new() -> Self {
        Self {
            config: obs::Config::load().map(Arc::new),
            ..Self::default()
        }
    }

    pub fn subscription(&self) -> Subscription<Event> {
        Subscription::batch([
            Subscription::run(portal::stream),
            self.config
                .clone()
                .map_or_else(Subscription::none, |config| {
                    Subscription::run_with(config, obs::stream)
                }),
        ])
    }

    pub fn tick_subscription(&self, visible: bool) -> Subscription<()> {
        if visible && self.obs.ticking() {
            cosmic::iced::time::every(Duration::from_secs(1)).map(|_| ())
        } else {
            Subscription::none()
        }
    }

    pub fn apply(&mut self, event: Event) {
        match event {
            Event::Sharing(sessions) => self.sessions = sessions,
            Event::Obs(snapshot) => self.obs = snapshot,
        }
    }

    pub fn active(&self) -> bool {
        !self.sessions.is_empty() || self.obs.active()
    }

    pub fn label(&self) -> Option<String> {
        if let Some(label) = self.obs.label() {
            return Some(label);
        }
        let first = self.sessions.first()?;
        let app = app_name(&first.app);
        Some(if self.sessions.len() == 1 {
            format!("{app} · Sharing")
        } else {
            format!("{app} +{} · Sharing", self.sessions.len() - 1)
        })
    }

    pub fn details(&self) -> Vec<String> {
        let mut details = self.obs.details();
        for session in &self.sessions {
            details.push(format!("{} · Sharing", app_name(&session.app)));
            details.extend(session.sources.iter().cloned());
        }
        details
    }

    pub fn icon(&self) -> &'static [u8] {
        if self.obs.active()
            || self
                .sessions
                .first()
                .is_some_and(|session| is_obs(&session.app))
        {
            ferese_theme::icons::OBS
        } else {
            ferese_theme::icons::DISPLAY
        }
    }

    pub fn bar_label(&self, compact: bool) -> String {
        self.obs.bar_label(compact).unwrap_or_else(|| {
            if self.sessions.len() > 1 {
                format!("Sharing +{}", self.sessions.len() - 1)
            } else {
                "Sharing".into()
            }
        })
    }
}

fn is_obs(app: &str) -> bool {
    matches!(app, "com.obsproject.Studio" | "obs" | "obs-studio")
}

fn app_name(app: &str) -> String {
    if is_obs(app) {
        "OBS".into()
    } else if app.is_empty() {
        "Screen capture".into()
    } else {
        app.chars().filter(|c| !c.is_control()).take(48).collect()
    }
}

fn configured(panel: &ferese_panel::Panel) -> bool {
    [&panel.start, &panel.center, &panel.end]
        .into_iter()
        .flat_map(|zone| &zone.groups)
        .flat_map(|group| &group.items)
        .any(|item| item.visible && item.kind == ferese_panel::ItemKind::CaptureStatus)
}

impl crate::FereseShell {
    pub(crate) fn capture_indicator(
        &self,
        size: u16,
        foreground: cosmic::iced::Color,
        opacity: f32,
    ) -> cosmic::Element<'_, cosmic::Action<crate::Message>> {
        use cosmic::iced::{Alignment, Background, Border};
        use cosmic::theme;
        use cosmic::widget::{Space, container, row};
        let mut indicator = row![crate::bar_icon(
            self.capture_status.icon(),
            size,
            foreground
        )]
        .spacing(4)
        .align_y(Alignment::Center);
        if self.capture_status.obs.on_air() {
            let color = self.config.theme.palette().error.scale_alpha(opacity);
            indicator = indicator.push(container(Space::new()).width(5).height(5).class(
                theme::Container::custom(move |_| container::Style {
                    background: Some(Background::Color(color)),
                    border: Border {
                        radius: 2.5.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            ));
        }
        indicator.into()
    }

    pub(crate) fn capture_item_enabled(&self) -> bool {
        self.config.panels.iter().any(configured)
    }

    pub(crate) fn capture_item_visible(&self, id: cosmic::iced::window::Id) -> bool {
        configured(self.panel_for_bar(id))
    }

    pub(crate) fn capture_timer_visible(&self) -> bool {
        use crate::panel_layout::Placement;
        use ferese_panel::morph::Mode;
        use ferese_panel::{ItemKind, Representation};
        if self.snapshot.binding_mode.is_some() {
            return false;
        }
        self.outputs
            .iter()
            .filter(|output| !output.hidden)
            .any(|output| {
                if self.menu.as_ref().is_some_and(|menu| {
                    menu.kind == crate::status_ui::Menu::CaptureStatus
                        && menu.anchor.parent == output.bar
                        && !menu.motion.closing()
                }) {
                    return true;
                }
                let panel = self.panel_for_bar(output.bar);
                if !configured(panel) {
                    return false;
                }
                if let Some(hub) = &output.runtime.hub {
                    let hub = hub.borrow();
                    return hub.menu == Some(crate::status_ui::Menu::CaptureStatus)
                        || matches!(hub.hub.mode(), Mode::Idle | Mode::Peek);
                }
                [&panel.start, &panel.center, &panel.end]
                    .into_iter()
                    .flat_map(|zone| &zone.groups)
                    .flat_map(|group| &group.items)
                    .filter(|item| item.visible && item.kind == ItemKind::CaptureStatus)
                    .any(|item| {
                        output.panel_resolution.as_ref().map_or(
                            item.representation.unwrap_or(Representation::Wide)
                                == Representation::Wide,
                            |resolution| {
                                matches!(
                                    resolution.items.get(&item.id),
                                    Some(Placement::Visible {
                                        representation: Representation::Wide,
                                        ..
                                    })
                                )
                            },
                        )
                    })
            })
    }

    pub(crate) fn view_capture_status(
        &self,
        representation: ferese_panel::Representation,
        selected: bool,
    ) -> cosmic::Element<'_, cosmic::Action<crate::Message>> {
        use cosmic::iced::{Alignment, Rectangle};
        use cosmic::theme;
        use cosmic::widget::{button, row};
        let palette = self.config.theme.for_bar();
        let metrics = crate::BarMetrics::from(self.config.panels[0].resolved_geometry());
        let foreground = crate::color(if selected {
            palette.accent
        } else {
            palette.text_primary
        });
        let label = self
            .capture_status
            .label()
            .unwrap_or_else(|| "Capture status".into());
        let mut content = row![self.capture_indicator(metrics.icon_size, foreground, 1.)]
            .spacing(4)
            .align_y(Alignment::Center);
        if representation != ferese_panel::Representation::Icon {
            content = content.push(
                self.render
                    .panel_text(
                        self.capture_status
                            .bar_label(representation == ferese_panel::Representation::Compact),
                    )
                    .class(theme::Text::Color(foreground)),
            );
        }
        let control = button::custom(crate::bar_content(content, metrics.group_item_height))
            .name(label)
            .padding([0., 6.])
            .height(metrics.group_item_height)
            .on_press_with_rectangle(|offset, bounds| {
                cosmic::Action::App(crate::Message::OpenMenu(
                    crate::status_ui::Menu::CaptureStatus,
                    Rectangle {
                        x: (bounds.x - offset.x).round() as i32,
                        y: (bounds.y - offset.y).round() as i32,
                        width: bounds.width.round() as i32,
                        height: bounds.height.round() as i32,
                    },
                ))
            });
        crate::motion::button(
            self.render,
            control,
            crate::color(palette.accent),
            selected,
            1.,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_visibility_and_representation_control_timer_work() {
        use ferese_panel::{ItemKind, Representation};
        let mut shell = crate::tests::shell_with_measured_panel();
        shell.outputs[0].panel_resolution = None;
        assert!(shell.capture_item_enabled());
        assert!(shell.capture_timer_visible());
        shell.outputs[0].hidden = true;
        assert!(!shell.capture_timer_visible());
        shell.outputs[0].hidden = false;
        for representation in [
            Representation::Compact,
            Representation::Icon,
            Representation::Wide,
        ] {
            let item = shell.config.panels[0]
                .end
                .groups
                .iter_mut()
                .flat_map(|g| &mut g.items)
                .find(|item| item.kind == ItemKind::CaptureStatus)
                .unwrap();
            item.representation = Some(representation);
            assert_eq!(
                shell.capture_timer_visible(),
                representation == Representation::Wide
            );
        }
        let item = shell.config.panels[0]
            .end
            .groups
            .iter_mut()
            .flat_map(|g| &mut g.items)
            .find(|item| item.kind == ItemKind::CaptureStatus)
            .unwrap();
        item.visible = false;
        assert!(!shell.capture_item_enabled());
        assert!(!shell.capture_timer_visible());
    }

    #[test]
    fn capture_does_not_claim_a_broadcast_and_idle_has_no_timer() {
        let mut model = Model::default();
        assert!(!model.active());
        assert_eq!(model.tick_subscription(true).units(), 0);
        model.apply(Event::Sharing(vec![Session {
            app: "com.obsproject.Studio".into(),
            sources: vec!["Workspace 1".into()],
        }]));
        assert_eq!(model.label().as_deref(), Some("OBS · Sharing"));
        assert_eq!(model.bar_label(false), "Sharing");
        assert_eq!(model.icon(), ferese_theme::icons::OBS);
        assert!(!model.obs.on_air());
        assert_eq!(model.tick_subscription(true).units(), 0);
        assert_eq!(model.details(), ["OBS · Sharing", "Workspace 1"]);
        model.apply(Event::Sharing(vec![]));
        assert!(!model.active());
        model.apply(Event::Sharing(vec![Session {
            app: "Firefox".into(),
            sources: vec!["Workspace 1".into()],
        }]));
        assert_eq!(model.icon(), ferese_theme::icons::DISPLAY);
        assert_eq!(model.bar_label(true), "Sharing");
        assert!(!model.obs.on_air());
    }
}
