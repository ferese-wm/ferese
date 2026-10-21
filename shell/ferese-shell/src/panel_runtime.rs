use std::time::{Duration, Instant};

use cosmic::app::Task;
use cosmic::iced::platform_specific::shell::commands::layer_surface::{
    set_exclusive_zone, set_input_zone, set_size,
};
use cosmic::iced::window;
use ferese_panel::adaptive::{Adaptive, WindowState};
use ferese_panel::{Panel, PanelStyle};

use crate::{FereseShell, Message, control::ShellSnapshot, output_bar_hidden};

pub(super) struct Runtime {
    pub(super) hub: Option<std::cell::RefCell<crate::panel_hub::Runtime>>,
    projection: Option<Panel>,
    adaptive: Adaptive,
    epoch: Instant,
    ticket: u64,
    scheduled: Option<Duration>,
}

impl Runtime {
    pub(super) fn new(panel: &Panel, snapshot: &ShellSnapshot, name: Option<&str>) -> Self {
        let adaptive = Adaptive::new(window_state(snapshot, name));
        let projection = project(panel, &adaptive);
        Self {
            hub: crate::panel_hub::new(panel),
            projection,
            adaptive,
            epoch: Instant::now(),
            ticket: 0,
            scheduled: None,
        }
    }

    pub(super) fn panel<'a>(&'a self, authored: &'a Panel) -> &'a Panel {
        self.projection.as_ref().unwrap_or(authored)
    }

    pub(super) fn reset(&mut self, panel: &Panel, snapshot: &ShellSnapshot, name: Option<&str>) {
        let ticket = self.ticket.wrapping_add(1);
        *self = Self::new(panel, snapshot, name);
        self.ticket = ticket;
    }

    pub(super) fn update_appearance(&mut self, panel: &Panel) {
        if let Some(hub) = &self.hub {
            hub.borrow_mut()
                .hub
                .pin_radii(panel.corner_radius.map(|r| r.0.map(f64::from)))
                .expect("validated panel radii");
        }
        if let Some(projection) = &mut self.projection {
            projection.border = panel.border;
            projection.background_opacity = panel.background_opacity;
            projection.corner_radius = panel.corner_radius;
        }
    }

    fn observe(&mut self, panel: &Panel, snapshot: &ShellSnapshot, name: Option<&str>) -> bool {
        if panel.style != PanelStyle::Adaptive {
            return false;
        }
        if self
            .adaptive
            .observe(window_state(snapshot, name), self.epoch.elapsed())
        {
            self.projection = project(panel, &self.adaptive);
            true
        } else {
            false
        }
    }

    fn schedule(&mut self) -> Option<(u64, Duration)> {
        let deadline = self.adaptive.deadline();
        if self.scheduled == deadline {
            return None;
        }
        self.scheduled = deadline;
        self.ticket = self.ticket.wrapping_add(1);
        deadline.map(|deadline| (self.ticket, deadline.saturating_sub(self.epoch.elapsed())))
    }
}

fn project(panel: &Panel, adaptive: &Adaptive) -> Option<Panel> {
    (panel.style == PanelStyle::Adaptive).then(|| {
        let mut projection = panel.clone();
        projection.style = adaptive.current().style().unwrap_or(PanelStyle::Floating);
        projection
    })
}

fn window_state(snapshot: &ShellSnapshot, name: Option<&str>) -> WindowState {
    let Some(output) = snapshot
        .outputs
        .iter()
        .find(|output| Some(output.name.as_str()) == name)
    else {
        return WindowState::default();
    };
    let mut state = WindowState::default();
    for window in snapshot
        .windows
        .iter()
        .filter(|window| window.workspace == output.active_workspace)
    {
        state.maximized |= window.maximized;
        state.fullscreen |= window.fullscreen;
    }
    state
}

impl FereseShell {
    pub(super) fn panel_for_bar(&self, id: window::Id) -> &Panel {
        self.outputs
            .iter()
            .find(|entry| entry.bar == id)
            .map_or(&self.config.panels[0], |entry| {
                entry.runtime.panel(&self.config.panels[0])
            })
    }

    pub(super) fn adaptive_deadline(&mut self, id: window::Id, ticket: u64) -> Task<Message> {
        if self
            .outputs
            .iter()
            .any(|entry| entry.bar == id && entry.runtime.ticket == ticket)
        {
            self.refresh_adaptive_panels()
        } else {
            Task::none()
        }
    }

    pub(super) fn refresh_adaptive_panels(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        let mut changed = Vec::new();
        for entry in &mut self.outputs {
            if entry.runtime.observe(
                &self.config.panels[0],
                &self.snapshot,
                entry.name.as_deref(),
            ) {
                let panel = entry.runtime.panel(&self.config.panels[0]);
                let geometry = panel.resolved_geometry();
                let output = self
                    .snapshot
                    .outputs
                    .iter()
                    .find(|output| Some(output.name.as_str()) == entry.name.as_deref())
                    .map(|output| output.id);
                entry.bar_regions.clear();
                entry.panel_resolution = None;
                tasks.push(set_input_zone(entry.bar, Some(Vec::new())));
                tasks.push(set_size(
                    entry.bar,
                    None,
                    Some(geometry.height.round() as u32),
                ));
                tasks.push(set_exclusive_zone(
                    entry.bar,
                    panel.exclusive_zone(output_bar_hidden(&self.snapshot, output)),
                ));
                changed.push(entry.bar);
            }
            if let Some((ticket, delay)) = entry.runtime.schedule() {
                let bar = entry.bar;
                tasks.push(Task::perform(
                    async move {
                        tokio::time::sleep(delay).await;
                    },
                    move |_| cosmic::Action::App(Message::AdaptiveDeadline(bar, ticket)),
                ));
            }
        }
        let geometry_changed = !changed.is_empty();
        for bar in changed {
            tasks.push(self.update_panel_margin(bar, true));
            tasks.push(self.dismiss_workspace_tooltip_for_bar(bar));
            if self
                .menu
                .as_ref()
                .is_some_and(|menu| menu.anchor.parent == bar)
            {
                tasks.push(self.destroy_menu());
            }
        }
        if geometry_changed {
            if let Some(surface) = self.notification_surface.take() {
                tasks.push(crate::destroy_layer_surface(surface.id));
            }
            tasks.push(self.sync_notification_surface());
        }
        Task::batch(tasks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_preserves_the_authored_composition_and_geometry_pins() {
        let mut panel = Panel::from_preset(PanelStyle::Adaptive);
        panel.geometry.side_margins = Some(24);
        let mut runtime = Runtime::new(&panel, &ShellSnapshot::default(), None);
        assert_eq!(runtime.panel(&panel).resolved_geometry().side_margins, 24);
        runtime.adaptive.observe(
            WindowState {
                maximized: true,
                fullscreen: false,
            },
            Duration::ZERO,
        );
        runtime.adaptive.observe(
            WindowState {
                maximized: true,
                fullscreen: false,
            },
            Duration::from_millis(150),
        );
        runtime.projection = project(&panel, &runtime.adaptive);
        assert_eq!(runtime.panel(&panel).resolved_geometry().side_margins, 0);
        assert_eq!(runtime.panel(&panel).start, panel.start);
        assert_eq!(panel.style, PanelStyle::Adaptive);
        assert_eq!(panel.geometry.side_margins, Some(24));
    }

    #[test]
    fn appearance_reload_updates_the_projection_without_resetting_adaptive_state() {
        let mut panel = Panel::from_preset(PanelStyle::Adaptive);
        let mut runtime = Runtime::new(&panel, &ShellSnapshot::default(), None);
        runtime.adaptive.observe(
            WindowState {
                maximized: true,
                fullscreen: false,
            },
            Duration::ZERO,
        );
        let pending = runtime.adaptive.deadline();
        panel.background_opacity = Some(0.4);
        panel.border = true;
        panel.corner_radius = Some(ferese_panel::CornerRadii::uniform(12.));
        runtime.update_appearance(&panel);
        assert_eq!(runtime.panel(&panel).background_opacity, Some(0.4));
        assert_eq!(runtime.panel(&panel).corner_radius, panel.corner_radius);
        assert!(runtime.panel(&panel).border);
        assert_eq!(runtime.adaptive.deadline(), pending);
    }

    #[test]
    fn reset_invalidates_a_pending_deadline() {
        let panel = Panel::from_preset(PanelStyle::Adaptive);
        let mut runtime = Runtime::new(&panel, &ShellSnapshot::default(), None);
        runtime.adaptive.observe(
            WindowState {
                maximized: true,
                fullscreen: false,
            },
            Duration::ZERO,
        );
        let (ticket, _) = runtime.schedule().unwrap();
        runtime.reset(&panel, &ShellSnapshot::default(), None);
        assert_ne!(runtime.ticket, ticket);
        assert_eq!(runtime.scheduled, None);
    }
}
