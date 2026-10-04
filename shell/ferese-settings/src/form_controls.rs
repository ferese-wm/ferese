use cosmic::widget::column;

use super::{
    Alignment, App, Element, Field, Kind, Length, Message, button, container, fonts, row, set, slider, text_input,
    visuals, widget,
};

impl App {
    pub(super) fn label<'a>(
        &self,
        text: impl Into<std::borrow::Cow<'a, str>> + 'a,
        size: f32,
    ) -> widget::Text<'a, cosmic::Theme, cosmic::Renderer> {
        ferese_theme::text(text, self.font).size(size)
    }

    pub(super) fn note(&self, text: &str) -> Element<'static, Message> {
        let palette = visuals::Palette::from_resolved(&self.resolved.presented);
        self.label(text.to_owned(), 11.)
            .class(cosmic::theme::Text::Color(palette.muted))
            .into()
    }

    pub(super) fn settings_button(
        &self,
        label: &str,
        icon: &str,
        message: Option<Message>,
        selected: bool,
    ) -> Element<'static, Message> {
        let palette = visuals::Palette::from_resolved(&self.resolved.presented);
        button::custom(
            row([])
                .spacing(5)
                .align_y(Alignment::Center)
                .push(visuals::action_icon(icon, palette.text))
                .push(self.label(label.to_owned(), 12.)),
        )
        .name(label.to_owned())
        .padding([4, 8])
        .class(visuals::button_style(palette, selected))
        .on_press_maybe(message)
        .into()
    }

    pub(super) fn settings_icon_button(
        &self,
        label: &str,
        icon: &str,
        message: Option<Message>,
    ) -> Element<'static, Message> {
        let palette = visuals::Palette::from_resolved(&self.resolved.presented);
        widget::tooltip(
            button::custom(visuals::action_icon(icon, palette.text))
                .name(label.to_owned())
                .padding(4)
                .class(visuals::button_style(palette, false))
                .on_press_maybe(message),
            self.label(label.to_owned(), 11.),
            widget::tooltip::Position::Top,
        )
        .into()
    }

    pub(super) fn field(&self, mut field: Field) -> Element<'static, Message> {
        if field.path == "theme.material.opacity" {
            let appearance = self.resolved.theme.appearance;
            let name = match appearance {
                ferese_config::theme::Appearance::Light => "light",
                ferese_config::theme::Appearance::Dark => "dark",
            };
            field.path = format!("theme.{name}.material.opacity");
            field.description = format!("Opacity for the current {name} appearance. Text and icons stay opaque.");
            if let Kind::Range { default, .. } = &mut field.kind {
                *default = self.resolved.theme.tokens.material.opacity;
            }
        }
        let palette = visuals::Palette::from_resolved(&self.resolved.presented);
        let mut labels = column([]).spacing(2).push(self.label(field.label.clone(), 13.));
        if !field.description.is_empty() {
            labels = labels.push(
                self.label(field.description.clone(), 11.)
                    .class(cosmic::theme::Text::Color(palette.muted)),
            );
        }
        let labels = labels.width(Length::Fill);
        let path = field.path.clone();
        let control: Element<'static, Message> = match field.kind.clone() {
            Kind::Font => {
                let value = self
                    .inputs
                    .get(&path)
                    .cloned()
                    .unwrap_or_else(|| self.draft.string(&path, ""));
                let families = fonts::families();
                let selected = if value.is_empty() {
                    Some(0)
                } else {
                    families.iter().position(|family| family == &value)
                };
                let selection_path = path.clone();
                let commit = field.clone();
                column([])
                    .spacing(4)
                    .push(
                        ferese_theme::controls::select(
                            cosmic::iced::widget::pick_list(
                                families,
                                selected.map(|index| families[index].clone()),
                                move |family: String| {
                                    let value = if family == "Default font" {
                                        String::new()
                                    } else {
                                        family
                                    };
                                    Message::SelectFont(selection_path.clone(), value)
                                },
                            )
                            .font(self.font)
                            .text_size(12)
                            .width(225),
                            palette,
                        )
                        .width(225),
                    )
                    .push(
                        text_input("Default font", value)
                            .font(self.font)
                            .padding([4, 8])
                            .style(visuals::input_style(palette))
                            .on_input(move |value| Message::Draft(path.clone(), value))
                            .on_submit(move |_| Message::Commit(field.clone()))
                            .on_unfocus(Message::Commit(commit))
                            .width(225)
                            .size(12),
                    )
                    .into()
            }
            Kind::Toggle(default) => {
                let enabled = self.draft.boolean(&path, default);
                ferese_theme::controls::switch(enabled, palette)
                    .name(format!("{}: {}", field.label, if enabled { "on" } else { "off" }))
                    .on_press(Message::Change(set(&path, !enabled)))
                    .into()
            }
            Kind::Range {
                default,
                min,
                max,
                step,
                suffix,
                integer,
            } => {
                let value = self
                    .ranges
                    .get(&path)
                    .copied()
                    .unwrap_or_else(|| self.draft.number(&path, default));
                let display = if integer || step >= 1. {
                    format!("{value:.0}{suffix}")
                } else {
                    format!("{value:.2}{suffix}")
                };
                let release = field.clone();
                row([])
                    .align_y(Alignment::Center)
                    .spacing(8)
                    .push(
                        slider(min..=max, value, move |value| Message::Range(field.clone(), value))
                            .step(step)
                            .class(ferese_theme::menus::slider(palette.accent, 1.))
                            .on_release(Message::Release(release))
                            .width(145),
                    )
                    .push(self.label(display, 12.).width(65))
                    .into()
            }
            Kind::Choice { default, choices } => {
                let value = self.wallpaper_field_value(&path, default);
                let mut options = row([]).spacing(2);
                for (key, label) in choices {
                    options = options.push(
                        button::custom(self.label(*label, 12.))
                            .padding([4, 8])
                            .class(visuals::button_style(palette, value == *key))
                            .on_press(Message::Change(set(&path, *key))),
                    );
                }
                container(options)
                    .padding(3)
                    .class(visuals::surface(palette.sidebar, 10.))
                    .into()
            }
            Kind::Text { default, argv } => {
                let value = self.inputs.get(&path).cloned().unwrap_or_else(|| {
                    if argv {
                        self.draft.argv(&path)
                    } else {
                        self.wallpaper_field_value(&path, default)
                    }
                });
                let commit = field.clone();
                let is_color = path.starts_with("theme.colors.");
                let swatch = visuals::color(&value, palette.accent);
                let input = text_input(default, value)
                    .font(self.font)
                    .padding([4, 8])
                    .style(visuals::input_style(palette))
                    .on_input(move |value| Message::Draft(path.clone(), value))
                    .on_submit(move |_| Message::Commit(field.clone()))
                    .on_unfocus(Message::Commit(commit))
                    .width(if is_color { 155 } else { 225 })
                    .size(12);
                if is_color {
                    row([])
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .push(container(widget::Space::new().width(24).height(24)).class(visuals::surface(swatch, 6.)))
                        .push(input)
                        .into()
                } else {
                    input.into()
                }
            }
        };
        container(
            row([])
                .spacing(10)
                .align_y(Alignment::Center)
                .push(labels)
                .push(control),
        )
        .padding([7, 10])
        .width(Length::Fill)
        .class(visuals::surface(palette.card, 0.))
        .into()
    }
}
