use cosmic::app::Task;
use cosmic::iced::{Alignment, Length, window};
use cosmic::widget::{button, container, row};
use cosmic::{Element, theme};
use ferese_theme::icons;

use super::{Event, Peek};
use crate::{FereseShell, Message, recording, status_ui};

impl FereseShell {
    pub(crate) fn protected_activity_slots(&self) -> usize {
        usize::from(self.hub_priority().is_some()) * 2
    }
    pub(super) fn hub_low_battery(&self) -> Option<u8> {
        self.status
            .battery
            .as_ref()
            .filter(|battery| {
                battery.percent < self.config.status.low_battery_threshold
                    && matches!(battery.status.as_str(), "Discharging" | "Pending discharge" | "Empty")
            })
            .map(|battery| battery.percent)
    }

    pub(super) fn hub_priority(&self) -> Option<Peek> {
        if matches!(
            self.recorder.state,
            recording::State::Recording(_) | recording::State::Saving
        ) {
            Some(Peek::Recording)
        } else {
            self.hub_low_battery().map(|_| Peek::Battery)
        }
    }

    pub(super) fn hub_peek_content(&self, id: window::Id) -> (Peek, f64) {
        if let Some(priority) = self.hub_priority() {
            return (
                priority,
                if priority == Peek::Recording && self.hub_low_battery().is_some() {
                    280.
                } else {
                    240.
                },
            );
        }
        if self.has_compact_activities(id) {
            let width = self
                .outputs
                .iter()
                .find(|entry| entry.bar == id)
                .and_then(|entry| entry.runtime.hub.as_ref())
                .and_then(|runtime| runtime.borrow().idle_content_width)
                .unwrap_or(self.panel_for_bar(id).morph.idle_width);
            let workspaces = self.hub_workspace_width(id);
            return (Peek::Date, width + if workspaces > 0. { workspaces + 10. } else { 0. });
        }
        let player = self.media.snapshot.selected.as_ref().filter(|player| {
            matches!(
                player.status,
                ferese_ipc::media::Playback::Playing | ferese_ipc::media::Playback::Paused
            )
        });
        let maximum = self.panel_for_bar(id).morph.peek_width.clamp(200., 280.);
        player.map_or_else(
            || {
                (
                    Peek::Date,
                    (self.clock.chars().count() as f64 * 7. + 54.).clamp(200., maximum),
                )
            },
            |player| {
                (
                    Peek::Media,
                    (player.label().chars().count() as f64 * 7. + 77.).clamp(200., maximum),
                )
            },
        )
    }

    pub(crate) fn sync_hub_activity(&mut self) -> Task<Message> {
        self.sync_activity_presentations();
        if self.snapshot.binding_mode.is_some() {
            return Task::none();
        }
        let priority = self.hub_priority();
        let changes: Vec<_> = self
            .outputs
            .iter()
            .filter(|entry| entry.runtime.hub.is_some())
            .map(|entry| (entry.bar, self.hub_peek_content(entry.bar)))
            .collect();
        Task::batch(changes.into_iter().map(|(id, peek)| {
            Task::batch([
                self.transition_hub(id, Event::Activity { priority, peek }),
                self.transition_hub(id, Event::Resize),
            ])
        }))
    }

    pub(super) fn hub_activity_strip(
        &self,
        id: window::Id,
        priority: Peek,
        expanded: bool,
        intrinsic: bool,
        opacity: f32,
    ) -> Element<'_, cosmic::Action<Message>> {
        let palette = self.config.theme.palette();
        let foreground = palette.text.scale_alpha(opacity);
        let warning = palette.error.scale_alpha(opacity);
        let width = if intrinsic || !expanded {
            Length::Shrink
        } else {
            Length::Fill
        };
        if priority == Peek::Battery {
            let percent = self
                .hub_low_battery()
                .or_else(|| self.status.battery.as_ref().map(|b| b.percent));
            let label = percent.map_or_else(
                || "Battery".into(),
                |percent| {
                    if expanded {
                        format!("Low battery · {percent}%")
                    } else {
                        format!("{percent}% battery")
                    }
                },
            );
            return row![
                icons::tinted(icons::BATTERY_25, 18, warning),
                self.render
                    .text(label)
                    .size(13)
                    .class(theme::Text::Color(foreground))
                    .width(width)
                    .align_x(if expanded { Alignment::End } else { Alignment::Start })
            ]
            .spacing(8)
            .align_y(Alignment::Center)
            .width(width)
            .into();
        }
        let elapsed = self.recorder.elapsed();
        let saving = matches!(self.recorder.state, recording::State::Saving);
        let mut strip = row::with_capacity(4)
            .spacing(8)
            .align_y(Alignment::Center)
            .width(width)
            .push(icons::tinted(
                if saving { icons::RECORD_SAVING } else { icons::RECORD },
                18,
                warning,
            ));
        if let Some(elapsed) = elapsed {
            if expanded {
                strip = strip.push(
                    self.render
                        .text("Recording")
                        .size(13)
                        .class(theme::Text::Color(foreground))
                        .width(Length::Fill),
                );
            }
            // Reserve the same width for every second, regardless of digit advances.
            let timer_width = elapsed.len().max(5) as f32 * 8.;
            strip = strip.push(
                container(self.render.text(elapsed).size(13).class(theme::Text::Color(foreground)))
                    .center_x(timer_width),
            );
        } else {
            strip = strip.push(
                self.render
                    .text(if saving && !expanded {
                        "Saving…"
                    } else if saving {
                        "Saving recording…"
                    } else {
                        "Recording ended"
                    })
                    .size(13)
                    .class(theme::Text::Color(foreground))
                    .width(width),
            );
        }
        if let Some(percent) = self.hub_low_battery() {
            let style = button::Style {
                text_color: Some(warning),
                ..Default::default()
            };
            strip = strip.push(
                button::custom(
                    row![
                        icons::tinted(icons::BATTERY_25, 16, warning),
                        self.render
                            .text(format!("{percent}%"))
                            .size(12)
                            .class(theme::Text::Color(warning))
                    ]
                    .spacing(4)
                    .align_y(Alignment::Center),
                )
                .name(format!("Low battery, {percent} percent. Open battery controls"))
                .padding(0)
                .height(28)
                .class(theme::Button::Custom {
                    active: Box::new(move |_, _| style),
                    hovered: Box::new(move |_, _| style),
                    pressed: Box::new(move |_, _| style),
                    disabled: Box::new(move |_| style),
                })
                .on_press(cosmic::Action::App(Message::HubOpen(id, status_ui::Menu::Battery))),
            );
        }
        strip.into()
    }
}
