use cosmic::iced::{Alignment, Length};
use cosmic::widget::{button, column, container, row, scrollable, text_input};
use cosmic::{Element, theme};
use ferese_theme::icons::symbolic as glyph;

use super::{Audio, Message, Picker, Source, SourceGroup};

impl Picker {
    pub(super) fn view_dialog(&self) -> Element<'_, Message> {
        let content = self.view_contents();
        if self.options_source.is_some() {
            cosmic::widget::popover::popover(super::backdrop::backdrop(
                content,
                self.options_dialog(),
            ))
            .modal(true)
            .popup(self.options_dialog())
            .into()
        } else {
            content
        }
    }

    pub(super) fn view_contents(&self) -> Element<'_, Message> {
        let app = if self.prompt.app.is_empty() {
            "this application"
        } else {
            &self.prompt.app
        };
        let title = if self.prompt.recording {
            "Record your screen"
        } else if self.prompt.window_capture {
            "Choose a window"
        } else {
            "Choose what to share"
        };
        let subtitle = if self.prompt.recording {
            "Saved to your Videos folder".to_owned()
        } else if self.prompt.window_capture {
            format!("Screenshot for {app}")
        } else {
            format!("Share with {app}")
        };
        let close = button::custom(glyph(ferese_theme::icons::CLOSE, 16))
            .class(self.control_style(false))
            .padding(8)
            .name("Close dialog")
            .on_press(Message::Cancel);
        let header = row![
            column![
                self.text(title).size(22),
                self.muted(subtitle).size(13).ellipsize(
                    cosmic::iced::widget::text::Ellipsize::End(
                        cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1)
                    )
                )
            ]
            .spacing(6)
            .width(Length::Fill),
            close,
        ]
        .spacing(16)
        .align_y(Alignment::Start);

        let mut content = column![header].spacing(18).height(Length::Fill);
        if SourceGroup::available(&self.prompt).count() > 1 {
            content = content.push(self.source_tabs());
        }
        if self.group.count(&self.prompt) > 6 {
            content = content.push(
                text_input(
                    format!("Search {}", self.group.label().to_lowercase()),
                    &self.query,
                )
                .font(self.font)
                .style(borderless_input(ferese_theme::controls::material_input(
                    self.palette,
                    self.background.a,
                )))
                .on_input(Message::Search),
            );
        }
        content = content.push(self.muted(self.group.description()).size(12));
        let empty = self.visible_sources().next().is_none();
        let mut sources = column([]).spacing(6).width(Length::Fill);
        for (index, source) in self.visible_sources() {
            sources = sources.push(self.source_row(index, source));
        }
        let source_list: Element<'_, Message> = if empty {
            container(self.muted("No matching sources").size(14))
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .into()
        } else {
            scrollable(sources)
                .direction(cosmic::iced::widget::scrollable::Direction::Vertical(
                    cosmic::iced::widget::scrollable::Scrollbar::hidden(),
                ))
                .height(Length::Fill)
                .width(Length::Fill)
                .into()
        };
        content = content.push(source_list);

        let selected = match self.selected.as_slice() {
            [] => "Choose a source to continue".to_owned(),
            [index] => self.source_title(&self.prompt.sources[*index]),
            selected => format!("{} sources selected", selected.len()),
        };
        let label = if self.prompt.recording {
            if self.region {
                "Select region".to_owned()
            } else {
                "Record".to_owned()
            }
        } else if self.prompt.window_capture {
            "Capture".to_owned()
        } else if self.selected.len() > 1 {
            format!("Share {} sources", self.selected.len())
        } else {
            "Share".to_owned()
        };
        let cancel = ferese_theme::controls::text_button("Cancel", self.font, self.palette, false)
            .class(self.control_style(false))
            .on_press(Message::Cancel);
        let share = ferese_theme::controls::text_button(label, self.font, self.palette, true)
            .class(borderless_button(ferese_theme::accent_button()))
            .on_press_maybe((!self.selected.is_empty()).then_some(Message::Share));
        let footer = row![
            self.muted(selected).size(12).width(Length::Fill).ellipsize(
                cosmic::iced::widget::text::Ellipsize::End(
                    cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1)
                )
            ),
            cancel,
            share,
        ]
        .spacing(12)
        .align_y(Alignment::Center);
        container(content.push(footer))
            .padding(24)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn source_tabs(&self) -> Element<'_, Message> {
        let mut tabs = row([]).spacing(4);
        for group in SourceGroup::available(&self.prompt) {
            let selected = self
                .selected
                .iter()
                .filter(|index| group.contains(&self.prompt.sources[**index]))
                .count();
            let label = if selected > 0 {
                format!("{} · {selected}", group.label())
            } else {
                group.label().to_owned()
            };
            tabs = tabs.push(
                button::custom(
                    container(self.text(label).size(13))
                        .center_x(Length::Fill)
                        .center_y(34),
                )
                .class(self.control_style(self.group == group))
                .padding(0)
                .width(Length::Fill)
                .on_press(Message::Group(group)),
            );
        }
        tabs.into()
    }

    fn source_row<'a>(&self, index: usize, source: &'a Source) -> Element<'a, Message> {
        let selected = self.selected.contains(&index);
        let icon = container(glyph(self.group.icon(), 22))
            .center_x(40)
            .center_y(40);
        let title = self
            .text(self.source_title(source))
            .size(14)
            .width(Length::Fill)
            .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
            ));
        let mut metadata =
            if source.is_monitor() && source.label != source.name && !source.label.is_empty() {
                format!("{} · {} × {}", source.name, source.width, source.height)
            } else {
                format!("{} × {}", source.width, source.height)
            };
        if source.workspace_id().is_some() {
            // Workspace discovery dimensions are logical; they are not the
            // selected stream's pixel resolution, especially at fractional scale.
            let quality = if self.prompt.workspace_quality {
                self.workspace_quality
            } else {
                crate::workspace::Quality::Balanced
            };
            metadata = format!("{} quality", quality.label());
        }
        let check: Element<'_, Message> = if selected {
            glyph(ferese_theme::icons::CHECK, 18).into()
        } else {
            cosmic::iced::widget::Space::new().width(18).into()
        };
        let select = button::custom(
            row![
                icon,
                column![title, self.muted(metadata).size(12)]
                    .spacing(4)
                    .width(Length::Fill),
                check
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        )
        .class(self.control_style(selected))
        .name(self.source_title(source))
        .padding([10, 12])
        .width(Length::Fill)
        .on_press(Message::Select(index));
        let mut entry = row![select]
            .spacing(8)
            .align_y(Alignment::Center)
            .width(Length::Fill);
        if self.source_has_options(index) {
            entry = entry.push(
                button::custom(glyph(ferese_theme::icons::SETTINGS, 18))
                    .class(self.control_style(false))
                    .name(format!("Options for {}", self.source_title(source)))
                    .padding(9)
                    .on_press(Message::OpenOptions(index)),
            );
        }
        entry.into()
    }

    fn source_title(&self, source: &Source) -> String {
        let title = if source.label.trim().is_empty() {
            &source.name
        } else {
            &source.label
        };
        title.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    pub(super) fn options_dialog(&self) -> Element<'_, Message> {
        let source =
            &self.prompt.sources[self.options_source.expect("options have a selected source")];
        let title = if self.prompt.recording {
            "Recording options"
        } else {
            "Sharing options"
        };
        let header = row![
            self.text(title).size(20).width(Length::Fill),
            button::custom(glyph(ferese_theme::icons::CLOSE, 16))
                .class(self.control_style(false))
                .name("Close options")
                .padding(7)
                .on_press(Message::CloseOptions)
        ]
        .spacing(12)
        .align_y(Alignment::Center);
        let mut content = column![
            header,
            self.muted(self.source_title(source))
                .size(12)
                .width(Length::Fill)
                .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                    cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1)
                )),
            self.options()
        ]
        .spacing(14);
        let workspaces = self
            .selected
            .iter()
            .filter(|index| self.prompt.sources[**index].workspace_id().is_some())
            .count();
        if self.options_workspace() && workspaces > 1 {
            content = content.push(
                self.muted(format!("Applies to all {workspaces} selected workspaces."))
                    .size(12),
            );
        }
        content = content.push(row![
            cosmic::iced::widget::Space::new().width(Length::Fill),
            ferese_theme::controls::text_button("Done", self.font, self.palette, true)
                .class(borderless_button(ferese_theme::accent_button()))
                .on_press(Message::CloseOptions),
        ]);
        let palette = self.palette;
        container(content)
            .padding(20)
            .width(440)
            .class(theme::Container::custom(move |_| {
                cosmic::widget::container::Style {
                    text_color: Some(palette.text),
                    border: cosmic::iced::Border {
                        radius: palette.radius.min(12.).into(),
                        ..Default::default()
                    },
                    shadow: cosmic::iced::Shadow {
                        color: cosmic::iced::Color::from_rgba(0., 0., 0., 0.25),
                        offset: cosmic::iced::Vector::new(0., 8.),
                        blur_radius: 24.,
                    },
                    ..Default::default()
                }
            }))
            .into()
    }

    fn options(&self) -> Element<'_, Message> {
        let mut options = column([]).spacing(16).width(Length::Fill);
        if self.options_workspace() && self.can_choose_quality() {
            let mut choices = row([]).spacing(6);
            for quality in [
                crate::workspace::Quality::Native,
                crate::workspace::Quality::High,
                crate::workspace::Quality::Balanced,
            ] {
                choices = choices.push(
                    ferese_theme::controls::text_button(
                        quality.label(),
                        self.font,
                        self.palette,
                        quality == self.workspace_quality,
                    )
                    .class(self.control_style(quality == self.workspace_quality))
                    .width(Length::Fill)
                    .on_press(Message::WorkspaceQuality(quality)),
                );
            }
            options = options.push(
                column![
                    self.text("Workspace quality").size(13),
                    choices,
                    self.muted(self.workspace_quality.description()).size(12)
                ]
                .spacing(6),
            );
        }
        if self.options_workspace() && self.can_include_shell() {
            options = options.push(self.option_switch(
                "Include shell",
                "Panels, launcher and notifications while the workspace is visible.",
                self.include_shell,
                Message::IncludeShell,
            ));
        }
        if self.options_monitor() && self.prompt.recording {
            options = options.push(self.option_switch(
                "Select a region",
                "Choose part of the display before recording.",
                self.region,
                Message::Region,
            ));
            let mut audio = column([]).spacing(6);
            for choices in [
                [Audio::Off, Audio::System],
                [Audio::Microphone, Audio::Both],
            ] {
                let mut choices_row = row([]).spacing(6);
                for choice in choices {
                    choices_row = choices_row.push(
                        ferese_theme::controls::text_button(
                            choice.label(),
                            self.font,
                            self.palette,
                            self.audio == choice,
                        )
                        .class(self.control_style(self.audio == choice))
                        .width(Length::Fill)
                        .on_press(Message::Audio(choice)),
                    );
                }
                audio = audio.push(choices_row);
            }
            options = options.push(column![self.text("Audio").size(13), audio].spacing(6));
        }
        if self.options_monitor() && self.can_remember() {
            options = options.push(self.option_switch(
                "Allow without asking",
                if self.prompt.persist_mode == 1 {
                    "While this app is running."
                } else {
                    "Remember permission for the selected displays."
                },
                self.remember,
                Message::Remember,
            ));
        }
        container(options).padding([4, 0]).into()
    }

    fn option_switch<'a>(
        &self,
        label: &'a str,
        description: &'a str,
        enabled: bool,
        message: Message,
    ) -> Element<'a, Message> {
        row![
            column![self.text(label).size(13), self.muted(description).size(12)]
                .spacing(4)
                .width(Length::Fill),
            ferese_theme::controls::switch(enabled, self.palette)
                .class(borderless_button(theme::Button::Transparent))
                .on_press(message)
        ]
        .spacing(16)
        .align_y(Alignment::Center)
        .into()
    }

    fn control_style(&self, selected: bool) -> theme::Button {
        borderless_button(ferese_theme::controls::material_button_style(
            self.palette,
            selected,
            self.background.a,
        ))
    }

    fn muted<'a>(
        &self,
        value: impl Into<std::borrow::Cow<'a, str>> + 'a,
    ) -> cosmic::widget::Text<'a, cosmic::Theme, cosmic::Renderer> {
        self.text(value)
            .class(theme::Text::Color(self.palette.muted))
    }
}

fn borderless_button(class: theme::Button) -> theme::Button {
    use cosmic::widget::button::Catalog;
    let class = std::rc::Rc::new(class);
    let (active, hovered, pressed) = (class.clone(), class.clone(), class.clone());
    let paint = |mut style: button::Style, focused: bool| {
        style.border_width = 0.;
        style.outline_width = 0.;
        // Keep keyboard focus visible through a fill, without an outline.
        if focused {
            style.overlay = Some(
                cosmic::iced::Color {
                    a: 0.12,
                    ..style.text_color.unwrap_or(cosmic::iced::Color::WHITE)
                }
                .into(),
            );
        }
        style
    };
    theme::Button::Custom {
        active: Box::new(move |focused, theme| {
            paint(theme.active(focused, false, &active), focused)
        }),
        hovered: Box::new(move |focused, theme| {
            paint(theme.hovered(focused, false, &hovered), focused)
        }),
        pressed: Box::new(move |focused, theme| {
            paint(theme.pressed(focused, false, &pressed), focused)
        }),
        disabled: Box::new(move |theme| paint(theme.disabled(&class), false)),
    }
}

fn borderless_input(class: theme::TextInput) -> theme::TextInput {
    use cosmic::widget::text_input::StyleSheet;
    let class = std::rc::Rc::new(class);
    let (active, hovered, focused, error) =
        (class.clone(), class.clone(), class.clone(), class.clone());
    let paint = |mut style: cosmic::widget::text_input::Appearance| {
        style.border_width = 0.;
        style
    };
    theme::TextInput::Custom {
        active: Box::new(move |theme| paint(theme.active(&active))),
        hovered: Box::new(move |theme| paint(theme.hovered(&hovered))),
        focused: Box::new(move |theme| paint(theme.focused(&focused))),
        error: Box::new(move |theme| paint(theme.error(&error))),
        disabled: Box::new(move |theme| paint(theme.disabled(&class))),
    }
}
