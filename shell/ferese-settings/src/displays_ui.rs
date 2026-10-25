use cosmic::Element;
use cosmic::iced::{Alignment, Length};
use cosmic::widget::{column, container, row};

use crate::displays::{self, Refresh};
use crate::{App, Message, schema, visuals};

impl App {
    pub(super) fn displays_view(&self) -> Element<'_, Message> {
        let palette = visuals::Palette::from_resolved(&self.resolved.presented);
        let targets = displays::targets(&self.draft, &self.displays);
        let selected = displays::selected(&targets, self.display_selection.as_deref()).cloned();
        let mut panel = column([]).spacing(6);
        panel = panel.push(
            row([])
                .spacing(8)
                .align_y(Alignment::Center)
                .push(visuals::action_icon(
                    "M3 4h18v13H3z M12 17v4 M8 21h8",
                    palette.accent,
                ))
                .push(
                    ferese_theme::controls::select(
                        cosmic::iced::widget::pick_list(
                            targets.clone(),
                            selected.clone(),
                            |target| Message::SelectDisplay(target.key),
                        )
                        .placeholder("Select display")
                        .font(self.font)
                        .text_size(12)
                        .padding([4, 8])
                        .width(Length::Fill),
                        palette,
                    )
                    .width(Length::Fill),
                )
                .push(self.settings_icon_button(
                    "Refresh displays",
                    "M20 7V3l-3 3a8 8 0 1 0 3 6 M20 3h-5",
                    Some(Message::RefreshDisplays),
                )),
        );
        if let Some(error) = &self.display_error {
            panel = panel.push(cosmic::widget::tooltip(
                self.note("Live display information unavailable"),
                self.label(error.clone(), 11.),
                cosmic::widget::tooltip::Position::Bottom,
            ));
        }
        if let Some(target) = selected {
            let live = self
                .displays
                .iter()
                .find(|d| d.connector == target.matcher || d.identity == target.matcher);
            if let Some(display) = live {
                let status = display
                    .current
                    .map(|mode| format!("{} × {} · {}", mode.width, mode.height, mode.label()))
                    .unwrap_or_else(|| "Off".into());
                panel = panel.push(self.note(&status));
            }
            if let Some(prefix) = target.prefix {
                let configured = self.draft.string(&format!("{prefix}.mode"), "");
                let automatic = self.draft.boolean(&format!("{prefix}.auto_refresh"), false);
                if let Some(display) = live {
                    let resolutions = displays::resolutions(display);
                    if !resolutions.is_empty() {
                        let path = prefix.clone();
                        panel = panel.push(
                            self.display_control(
                                "Resolution",
                                ferese_theme::controls::select(
                                    cosmic::iced::widget::pick_list(
                                        resolutions,
                                        displays::resolution(&configured, display),
                                        move |size| Message::DisplayResolution(path.clone(), size),
                                    )
                                    .font(self.font)
                                    .text_size(12)
                                    .padding([4, 8])
                                    .width(225),
                                    palette,
                                )
                                .width(225)
                                .into(),
                            ),
                        );
                    } else {
                        panel = panel.push(self.display_mode_field(&prefix));
                    }
                    let modes = displays::choices(display, &configured);
                    let mut refresh: Vec<_> = modes.iter().copied().map(Refresh::Manual).collect();
                    if modes
                        .iter()
                        .any(|mode| (59_000..=61_000).contains(&mode.refresh))
                        && let Some(high) = modes.last()
                    {
                        refresh.push(Refresh::Automatic(*high));
                    }
                    if !refresh.is_empty() {
                        let configured_rate = configured
                            .split('@')
                            .nth(1)
                            .and_then(|r| r.parse::<f64>().ok())
                            .map(|r| r * 1000.)
                            .or_else(|| display.current.map(|m| f64::from(m.refresh)));
                        let selected = refresh.iter().copied().find(|value| match value {
                            Refresh::Automatic(_) => automatic,
                            Refresh::Manual(mode) => {
                                !automatic
                                    && configured_rate
                                        .is_some_and(|r| (r - f64::from(mode.refresh)).abs() < 1.)
                            }
                        });
                        let path = prefix.clone();
                        panel = panel.push(
                            self.display_control(
                                "Refresh rate",
                                ferese_theme::controls::select(
                                    cosmic::iced::widget::pick_list(
                                        refresh,
                                        selected,
                                        move |value| match value {
                                            Refresh::Manual(mode) => {
                                                Message::RefreshRate(path.clone(), mode, false)
                                            }
                                            Refresh::Automatic(mode) => {
                                                Message::RefreshRate(path.clone(), mode, true)
                                            }
                                        },
                                    )
                                    .font(self.font)
                                    .text_size(12)
                                    .padding([4, 8])
                                    .width(225),
                                    palette,
                                )
                                .width(225)
                                .into(),
                            ),
                        );
                    }
                } else {
                    panel = panel.push(self.display_mode_field(&prefix));
                }
                panel = panel.push(self.field(schema::range(
                    format!("{prefix}.scale"),
                    "Scale",
                    "",
                    schema::RangeSpec {
                        default: 1.,
                        min: 0.75,
                        max: 3.,
                        step: 0.05,
                        suffix: "×",
                        integer: false,
                    },
                )));
                panel = panel.push(row([]).spacing(5).align_y(Alignment::Center)
                    .push(cosmic::widget::tooltip(visuals::action_icon("M12 3a9 9 0 1 0 0 18a9 9 0 0 0 0-18 M12 11v6 M12 7h.01", palette.muted),
                        self.label("Auto uses 60 Hz below 30% battery and returns to normal on AC or at 35%. Applying a display mode can briefly blank the screen.", 11.), cosmic::widget::tooltip::Position::Top))
                    .push(self.note(if target.active { "Changes apply live" } else { "Saved profile · applies when active" })));
            } else {
                panel = panel.push(self.note("Managed automatically · no saved display profile"));
            }
        } else {
            panel = panel.push(self.note("No displays or saved profiles found"));
        }
        container(panel)
            .padding([7, 10])
            .width(Length::Fill)
            .class(visuals::surface(palette.card, 14.))
            .into()
    }

    fn display_control<'a>(
        &self,
        label: &str,
        control: Element<'a, Message>,
    ) -> Element<'a, Message> {
        row([])
            .spacing(10)
            .align_y(Alignment::Center)
            .padding([7, 10])
            .push(self.label(label.to_owned(), 13.).width(Length::Fill))
            .push(control)
            .into()
    }

    fn display_mode_field(&self, prefix: &str) -> Element<'static, Message> {
        self.field(schema::text(
            format!("{prefix}.mode"),
            "Resolution / refresh",
            "",
            "",
        ))
    }
}
