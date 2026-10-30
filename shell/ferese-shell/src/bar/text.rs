use super::{Color, Element, FereseShell, Message, focused_bar_title, status_ui, window};
use ferese_theme::text_bar::{self, Action, Model, Palette, Workspace};

impl FereseShell {
    pub(super) fn text_bar_model(&self, id: window::Id) -> Model {
        let output = self.output_for_bar(id);
        let mut workspaces: Vec<_> = self
            .snapshot
            .workspaces_for_output(output.map(|output| output.id))
            .collect();
        workspaces.sort_by_key(|workspace| workspace.index);
        let cells = workspaces
            .into_iter()
            .map(|workspace| Workspace {
                index: workspace.index,
                id: Some(workspace.id),
                active: output.is_some_and(|output| output.active_workspace == workspace.id),
                occupied: workspace.window_count > 0,
                urgent: self
                    .snapshot
                    .windows
                    .iter()
                    .any(|window| window.workspace == workspace.id && window.urgent),
            })
            .collect();
        let layout = output.and_then(|output| self.snapshot.layouts.get(&output.active_workspace));
        Model {
            workspaces: cells,
            layout: match layout.map(String::as_str) {
                Some("scrolling") => "scroll",
                Some("tree") => "[]=",
                _ => "?",
            }
            .into(),
            title: self.snapshot.binding_mode.as_ref().map_or_else(
                || focused_bar_title(&self.snapshot, output).to_owned(),
                |name| format!("mode: {name} · Esc to exit"),
            ),
            cpu: self
                .system_stats
                .cpu
                .map(|percent| format!("cpu {percent}%")),
            network: self.status.network.as_ref().map(|network| {
                if !network.enabled {
                    "wifi off".into()
                } else if network.connection.is_some() {
                    format!("wifi {}%", network.signal)
                } else {
                    "wifi offline".into()
                }
            }),
            throughput: self
                .system_stats
                .network
                .as_ref()
                .map(crate::system_stats::NetworkRate::label),
            disk: self
                .system_stats
                .disk
                .map(|percent| format!("disk {percent}%")),
            volume: self.status.audio.as_ref().map(|audio| {
                if audio.muted {
                    "vol muted".into()
                } else {
                    format!("vol {}%", audio.volume)
                }
            }),
            battery: self
                .status
                .battery
                .as_ref()
                .map(|battery| format!("bat {}%", battery.percent)),
            clock: self.clock.clone(),
            capture: self
                .capture_status
                .active()
                .then(|| (self.capture_status.bar_label(true), Action::Capture))
                .into_iter()
                .chain(self.recorder.busy().then(|| {
                    (
                        match self.recorder.state {
                            crate::recording::State::Selecting => "record setup",
                            crate::recording::State::Saving => "saving",
                            _ => "REC",
                        }
                        .into(),
                        Action::Recording,
                    )
                }))
                .collect(),
            interactive: self.snapshot.binding_mode.is_none(),
        }
    }

    pub(super) fn view_text_bar(&self, id: window::Id) -> Element<'_, cosmic::Action<Message>> {
        let panel = self.panel_for_bar(id);
        let output = self.outputs.iter().find(|entry| entry.bar == id);
        let effects = output.and_then(|entry| entry.effects.as_ref());
        let content = text_bar::view(
            self.text_bar_model(id),
            self.render.font,
            Palette::new(self.config.theme.appearance),
            move |action, rectangle| {
                if let Action::Workspace(workspace) = action {
                    return cosmic::Action::App(Message::ActivateWorkspace(workspace));
                }
                let menu = match action {
                    Action::Workspaces => status_ui::Menu::Workspaces,
                    Action::Recording => status_ui::Menu::Recording,
                    Action::Network => status_ui::Menu::Network,
                    Action::Audio => status_ui::Menu::Audio,
                    Action::Battery => status_ui::Menu::Battery,
                    Action::Calendar => status_ui::Menu::Calendar,
                    Action::Capture => status_ui::Menu::CaptureStatus,
                    _ => status_ui::Menu::System,
                };
                cosmic::Action::App(Message::OpenPopover(
                    menu,
                    status_ui::PopoverAnchor {
                        parent: id,
                        panel: panel.id.clone(),
                        item: None,
                        rectangle,
                    },
                ))
            },
        );
        // Reuse measured input regions, while the client paints an opaque flat bar.
        ferese_theme::panel_surface::frame(
            content,
            ferese_theme::panel_surface::Options {
                parameters: panel.shape_parameters(),
                bounds: None,
                groups: Vec::new(),
                scale: output.map_or(1., |entry| entry.scale),
                high_contrast: false,
                background: Color::TRANSPARENT,
                border: Color::TRANSPARENT,
                divider: Color::TRANSPARENT,
                native: true,
            },
            move |_| {
                if let Some(effects) = effects
                    && let Err(error) = effects.clear_panel_material()
                {
                    eprintln!("ferese-shell: could not clear text bar material: {error}");
                }
            },
            Some(Box::new(move |scene| {
                cosmic::Action::App(Message::PanelSurfaceChanged(id, Box::new(scene.clone())))
            })),
        )
    }
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
