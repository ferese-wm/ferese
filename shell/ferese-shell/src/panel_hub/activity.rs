use cosmic::app::Task;
use cosmic::iced::{Alignment, Length, window};
use cosmic::widget::{button, container, row};
use cosmic::{Element, theme};
use ferese_panel::morph::Mode;
use ferese_theme::icons;

use super::Peek;
use crate::{FereseShell, Message, recording, status_ui, text};

impl FereseShell {
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
        let priority = self.hub_priority();
        let changes: Vec<_> = self
            .outputs
            .iter()
            .filter_map(|entry| {
                let (peek, width) = self.hub_peek_content(entry.bar);
                let mut runtime = entry.runtime.hub.as_ref()?.borrow_mut();
                if runtime.priority == priority {
                    return None;
                }
                runtime.priority = priority;
                let mode = runtime.hub.mode();
                if mode.expanded() {
                    return None;
                }
                if mode == Mode::Peek || (mode == Mode::Toast && priority.is_some()) {
                    runtime.workspace_feedback = None;
                    runtime.previous_peek = runtime.peek;
                    runtime.peek = peek;
                    runtime.peek_width = width;
                    if mode == Mode::Peek {
                        runtime.hub.replace_content();
                    }
                    let target = if runtime.hovered { Mode::Peek } else { Mode::Idle };
                    Some((entry.bar, target))
                } else {
                    None
                }
            })
            .collect();
        Task::batch(changes.into_iter().map(|(id, mode)| self.set_hub_mode(id, mode, None)))
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
                text(label).size(13).class(theme::Text::Color(foreground)).width(width)
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
                    text("Recording")
                        .size(13)
                        .class(theme::Text::Color(foreground))
                        .width(Length::Fill),
                );
            }
            // Reserve the same width for every second, regardless of digit advances.
            let timer_width = elapsed.len().max(5) as f32 * 8.;
            strip = strip
                .push(container(text(elapsed).size(13).class(theme::Text::Color(foreground))).center_x(timer_width));
        } else {
            strip = strip.push(
                text(if saving {
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
                        text(format!("{percent}%")).size(12).class(theme::Text::Color(warning))
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
