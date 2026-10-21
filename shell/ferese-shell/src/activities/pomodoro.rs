//! Pomodoro uses the same type, material, and animated controls as status popups.
use crate::{FereseShell, Message, color_with_opacity, motion};
use cosmic::iced::{Alignment, Length, window};
use cosmic::widget::{button, column, row, tooltip};
use cosmic::{Element, theme};
use ferese_activities::pomodoro::{Phase, Status};
use ferese_ipc::activities::{PomodoroAction as Action, now_ms};
use ferese_theme::{icons, menus};

impl FereseShell {
    pub(super) fn pomodoro_controls(
        &self,
        bar: window::Id,
        opacity: f32,
    ) -> Element<'_, cosmic::Action<Message>> {
        let palette = self.config.theme;
        let primary = color_with_opacity(palette.text_primary, opacity);
        let muted = color_with_opacity(palette.text_muted, opacity);
        let accent = color_with_opacity(palette.accent, opacity);
        let current = self.activities.snapshot.pomodoro.as_ref();
        let session = current.map(|current| current.id.clone());
        let settings = current.map_or(&self.config.activities.pomodoro, |current| {
            &current.timer.settings
        });
        let (phase, seconds, description, label, action, symbol) = match current {
            None => (
                "Focus timer",
                u64::from(settings.work_minutes) * 60,
                format!(
                    "{} interval{} · {} min breaks",
                    settings.intervals,
                    if settings.intervals == 1 { "" } else { "s" },
                    settings.short_break_minutes
                ),
                "Start Pomodoro",
                Action::Start,
                icons::MEDIA_PLAY,
            ),
            Some(current) => {
                let timer = &current.timer;
                let phase = match timer.phase {
                    Phase::Work => "Focus",
                    Phase::ShortBreak => "Short break",
                    Phase::LongBreak => "Long break",
                };
                let description = if timer.status == Status::Finished {
                    format!(
                        "{} focus interval{} complete",
                        timer.completed_work,
                        if timer.completed_work == 1 { "" } else { "s" }
                    )
                } else {
                    let interval = if timer.phase == Phase::Work {
                        format!(
                            "Interval {} of {}",
                            timer
                                .completed_work
                                .saturating_add(1)
                                .min(settings.intervals),
                            settings.intervals
                        )
                    } else {
                        format!(
                            "{} of {} complete",
                            timer.completed_work, settings.intervals
                        )
                    };
                    format!(
                        "{}{interval}",
                        match timer.status {
                            Status::Paused => "Paused · ",
                            Status::Ready => "Ready · ",
                            _ => "",
                        },
                    )
                };
                let (label, action, symbol) = match timer.status {
                    Status::Running => ("Pause", Action::Pause, icons::MEDIA_PAUSE),
                    Status::Paused => ("Resume", Action::Resume, icons::MEDIA_PLAY),
                    Status::Ready => (
                        if timer.phase == Phase::Work {
                            "Start focus"
                        } else {
                            "Start break"
                        },
                        Action::Resume,
                        icons::MEDIA_PLAY,
                    ),
                    Status::Finished => ("Start again", Action::Start, icons::MEDIA_PLAY),
                };
                (
                    if timer.status == Status::Finished {
                        "Complete"
                    } else {
                        phase
                    },
                    timer.remaining(now_ms()).div_ceil(1_000),
                    description,
                    label,
                    action,
                    symbol,
                )
            }
        };
        let message = |action| cosmic::Action::App(Message::Pomodoro(action, session.clone()));
        let badge = menus::badge(
            icons::tinted(
                if current.is_some_and(|current| current.timer.status == Status::Finished) {
                    icons::CHECK
                } else {
                    icons::ACTIVITY_TIMER
                },
                20,
                accent,
            )
            .into(),
            crate::color(palette.accent),
            opacity,
            self.render.radius(18.),
        );
        let heading = row![
            badge,
            column![
                self.render
                    .text("Pomodoro")
                    .size(13)
                    .class(theme::Text::Color(primary)),
                self.render
                    .text(phase)
                    .size(11)
                    .class(theme::Text::Color(muted)),
            ]
            .spacing(3)
            .width(Length::Fill),
            self.render
                .text(format!("{:02}:{:02}", seconds / 60, seconds % 60))
                .size(26)
                .width(96)
                .align_x(cosmic::iced::alignment::Horizontal::Right)
                .wrapping(cosmic::iced::widget::text::Wrapping::None)
                .class(theme::Text::Color(primary)),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let primary_button = button::custom(
            row![
                icons::tinted(symbol, 16, accent),
                self.render.text(label).size(13)
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .id(session.as_ref().map_or_else(
            || cosmic::widget::Id::new(format!("pomodoro-start-{bar:?}")),
            |id| super::activity_widget_id(bar, id),
        ))
        .name(label)
        .padding([7, 10])
        .width(Length::Fill)
        .on_press(message(action));
        let mut controls = row![motion::button(
            self.render,
            primary_button,
            accent,
            true,
            opacity
        )]
        .spacing(4)
        .align_y(Alignment::Center);
        let secondary = |label: &'static str, symbol, action| {
            let control = button::custom(icons::tinted(symbol, 16, primary))
                .name(label)
                .padding(8)
                .on_press(message(action));
            tooltip(
                motion::button(self.render, control, primary, false, opacity),
                self.render
                    .text(label)
                    .size(12)
                    .class(theme::Text::Color(primary)),
                cosmic::widget::tooltip::Position::Top,
            )
        };
        if let Some(current) = current {
            if matches!(current.timer.status, Status::Running | Status::Paused) {
                controls = controls.push(secondary("Skip", icons::MEDIA_NEXT, Action::Next));
            }
            if current.timer.status != Status::Finished {
                controls = controls.push(secondary("Reset", icons::RESTART, Action::Reset));
            }
            controls = controls.push(secondary(
                if current.timer.status == Status::Finished {
                    "Dismiss"
                } else {
                    "Stop"
                },
                icons::RECORD_STOP,
                Action::Stop,
            ));
        }
        menus::section(
            column![
                heading,
                self.render
                    .text(description)
                    .size(11)
                    .class(theme::Text::Color(muted)),
                controls,
            ]
            .spacing(10)
            .width(Length::Fill)
            .into(),
            crate::color(palette.text_primary),
            opacity,
            self.render.radius(22.),
        )
    }
}
