use super::{
    Border, Category, Color, ContentFit, Element, FereseShell, Length, Message, Row, Target,
    bar_icon, button, color_with_opacity, container, ferese_surface_effects_v1, image, motion, row,
    theme, window,
};
use cosmic::iced::border::Shape;
use cosmic::iced::widget::{Space, mouse_area, opaque, rich_text, span, stack};
use cosmic::widget::{column, text_input};

type Ui<'a> = Element<'a, cosmic::Action<super::super::Message>>;
fn message(message: Message) -> cosmic::Action<super::super::Message> {
    cosmic::Action::App(super::super::Message::Launcher(message))
}
fn fill(color: Color, radius: f32) -> theme::Container<'static> {
    theme::Container::custom(move |_| container::Style {
        background: Some(color.into()),
        border: Border {
            radius: radius.into(),
            shape: Shape::Continuous,
            ..Default::default()
        },
        ..Default::default()
    })
}
fn button_style(color: Color, high_contrast: bool) -> theme::Button {
    button_radius(color, 20., high_contrast)
}
fn button_radius(color: Color, radius: f32, high_contrast: bool) -> theme::Button {
    let paint = move |theme: &cosmic::Theme, focused, hovered| {
        let color = if focused && color.a == 0. {
            Color::from(theme.cosmic().accent.base)
        } else {
            color
        };
        button::Style {
            background: Some(
                color
                    .scale_alpha(if high_contrast {
                        0.35
                    } else if hovered {
                        0.24
                    } else {
                        0.16
                    })
                    .into(),
            ),
            border_radius: radius.into(),
            border_width: if focused || color.a > 0. {
                if high_contrast { 2. } else { 1. }
            } else {
                0.
            },
            border_color: color,
            shape: Some(Shape::Continuous),
            ..Default::default()
        }
    };
    theme::Button::Custom {
        active: Box::new(move |focused, theme| paint(theme, focused, false)),
        hovered: Box::new(move |focused, theme| paint(theme, focused, true)),
        pressed: Box::new(move |focused, theme| paint(theme, focused, true)),
        disabled: Box::new(move |theme| paint(theme, false, false)),
    }
}
impl FereseShell {
    pub(super) fn view_launcher(&self, id: window::Id) -> Ui<'_> {
        let Some(launcher) = self.launcher.as_ref().filter(|launcher| launcher.id == id) else {
            return Space::new().into();
        };
        launcher.animation.borrow_mut().retarget(
            launcher.target_height(),
            launcher.closing,
            self.render.motion,
            super::Instant::now(),
        );
        let revision = Some(launcher.animation.borrow().started);
        motion::geometry_driven(
            revision,
            move |now| self.launcher_content(id, now),
            move |now| launcher.animation.borrow().sample(now).active(),
            move |now| {
                if let Some(effects) = &launcher.effects {
                    let _ = effects.set_presentation(
                        &launcher.regions.lock().expect(
                            "surface rendering state must remain consistent across callbacks",
                        ),
                        launcher.animation.borrow().sample(now).progress.current as f32,
                        [],
                        ferese_surface_effects_v1::Role::Popover,
                    );
                }
            },
            move |now| {
                (launcher.closing
                    && !launcher
                        .animation
                        .borrow()
                        .sample(now)
                        .progress
                        .is_animating())
                .then_some(message(Message::Closed(id)))
            },
            None,
        )
    }
    fn launcher_content(&self, id: window::Id, now: super::Instant) -> Ui<'_> {
        let Some(launcher) = self.launcher.as_ref().filter(|l| l.id == id) else {
            return Space::new().into();
        };
        let sample = launcher.animation.borrow().sample(now);
        let palette = self.config.theme.palette();
        let opacity = sample.progress.current as f32;
        let text = palette.text.scale_alpha(opacity);
        let muted = palette.muted.scale_alpha(opacity);
        let appearance = move |_: &cosmic::Theme| text_input::Appearance {
            shape: Some(Shape::Continuous),
            outline: None,
            background: Color::TRANSPARENT.into(),
            border_radius: 0.into(),
            border_offset: None,
            border_width: 0.,
            border_color: Color::TRANSPARENT,
            label_color: text,
            placeholder_color: muted,
            selected_text_color: text,
            icon_color: Some(text),
            text_color: Some(text),
            selected_fill: palette.accent.scale_alpha(0.3 * opacity),
        };
        let accent = palette.accent.scale_alpha(opacity);
        let high_contrast = self.config.theme.high_contrast;
        let medium = cosmic::font::Font {
            weight: cosmic::iced::font::Weight::Medium,
            ..self.render.font
        };
        let placeholder = launcher.category.map_or("Search", Category::label);
        let input = text_input(placeholder, &launcher.query)
            .id(launcher.input.clone())
            .font(self.render.font)
            .size(24)
            .line_height(cosmic::iced::widget::text::LineHeight::Absolute(32.into()))
            .padding(0)
            .style(theme::TextInput::Custom {
                active: Box::new(appearance),
                error: Box::new(appearance),
                hovered: Box::new(appearance),
                focused: Box::new(appearance),
                disabled: Box::new(appearance),
            });
        let input = if launcher.activating {
            input
        } else {
            input.on_input(move |value| message(Message::Input(id, value)))
        };
        let completion = launcher
            .selected_row()
            .filter(|_| {
                !launcher.query.is_empty()
                    && launcher.ghost_visible
                    && launcher.query.chars().count() <= 28
            })
            .map(|row| {
                if row
                    .title
                    .get(..launcher.query.len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&launcher.query))
                {
                    std::borrow::Cow::Borrowed(&row.title[launcher.query.len()..])
                } else {
                    std::borrow::Cow::Owned(format!("  {}", row.title))
                }
            });
        let show_tab = completion.is_some();
        let completion_text = rich_text([
            span::<(), cosmic::font::Font>(&launcher.query).color(Color::TRANSPARENT),
            span(completion.unwrap_or_default()).color(text.scale_alpha(0.3)),
        ])
        .size(24)
        .line_height(cosmic::iced::widget::text::LineHeight::Absolute(32.into()))
        .wrapping(cosmic::iced::widget::text::Wrapping::None)
        .font(self.render.font);
        let mut suggestion = row![completion_text]
            .spacing(6)
            .align_y(cosmic::iced::Alignment::Center);
        if show_tab {
            suggestion = suggestion.push(
                container(
                    self.render
                        .text("Tab")
                        .size(12)
                        .line_height(cosmic::iced::widget::text::LineHeight::Absolute(14.into()))
                        .class(theme::Text::Color(muted)),
                )
                .padding([2, 6])
                .class(fill(text.scale_alpha(0.06), 5.)),
            );
        }
        // The input determines the layout even when the completion is empty.
        // Keep both layers present so result updates preserve focus and cursor state.
        let editor: Ui<'_> = stack![input]
            .push_under(suggestion)
            .width(Length::Fill)
            .clip(true)
            .into();
        let search_match = launcher
            .selected_row()
            .filter(|_| !launcher.query.is_empty());
        let icon_category = search_match.map_or(launcher.category, Row::category);
        let icon = match icon_category {
            Some(Category::Windows) => ferese_theme::icons::WINDOW,
            Some(Category::Apps) => ferese_theme::icons::APPS,
            Some(Category::Files) => ferese_theme::icons::FOLDER,
            None if search_match.is_some() => ferese_theme::icons::morph::CONTROLS,
            None => ferese_theme::icons::SEARCH,
        };
        let icon_color = if search_match.is_some() || launcher.category.is_some() {
            accent
        } else {
            muted
        };
        let mut input_row = row![bar_icon(icon, 24, icon_color), editor]
            .spacing(16)
            .align_y(cosmic::iced::Alignment::Center);
        if let Some(handle) = search_match.and_then(|result| result.icon.clone()) {
            input_row =
                input_row.push(cosmic::widget::icon::icon(handle).size(28).opacity(opacity));
        }
        if launcher.query.is_empty() && launcher.category.is_none() {
            let dark = self.config.theme.appearance == ferese_config::theme::Appearance::Dark;
            let mut pills = row![].spacing(6);
            let category_iter = [Category::Apps, Category::Files, Category::Windows]
                .into_iter()
                .enumerate();
            for (index, category) in category_iter {
                let symbol = match category {
                    Category::Apps => ferese_theme::icons::APPS,
                    Category::Files => ferese_theme::icons::FOLDER,
                    Category::Windows => ferese_theme::icons::WINDOW,
                };
                let selected = launcher.category_focused && launcher.selected == index;
                let normal = if dark { 0.065 } else { 0.04 };
                let strong = if dark { 0.10 } else { 0.07 };
                let paint = move |alpha| button::Style {
                    background: Some(text.scale_alpha(alpha).into()),
                    border_radius: 16.into(),
                    border_width: 0.,
                    ..Default::default()
                };
                let label = self
                    .render
                    .text(category.label())
                    .font(medium)
                    .size(13)
                    .line_height(cosmic::iced::widget::text::LineHeight::Absolute(18.into()))
                    .class(theme::Text::Color(text));
                pills = pills.push(
                    button::custom(
                        row![bar_icon(symbol, 16, accent), label]
                            .spacing(7)
                            .align_y(cosmic::iced::Alignment::Center),
                    )
                    .height(32)
                    .padding(cosmic::iced::Padding {
                        top: 7.,
                        bottom: 7.,
                        left: 10.,
                        right: 13.,
                    })
                    .class(theme::Button::Custom {
                        active: Box::new(move |_, _| paint(if selected { strong } else { normal })),
                        hovered: Box::new(move |_, _| paint(strong)),
                        pressed: Box::new(move |_, _| paint(strong - 0.02)),
                        disabled: Box::new(move |_| paint(normal)),
                    })
                    .on_press(message(Message::Browse(id, category))),
                );
            }
            input_row = input_row.push(pills);
        }
        let input_content = if let Some(error) = launcher.status_message() {
            column![
                input_row,
                self.render
                    .text(error)
                    .size(12)
                    .class(theme::Text::Color(muted))
            ]
            .spacing(2)
        } else {
            column![input_row]
        };
        let mut body = column![
            container(input_content)
                .height(72)
                .padding(cosmic::iced::Padding {
                    left: 24.,
                    right: 20.,
                    ..Default::default()
                })
                .align_y(cosmic::iced::Alignment::Center)
        ];
        if launcher.apps_browse() {
            let columns = launcher.grid_columns();
            let visible_rows = launcher.grid_rows();
            let total = launcher.rows.len().div_ceil(columns);
            let first = launcher
                .apps_first_row
                .min(total.saturating_sub(1))
                .saturating_sub(1);
            let end = (first + visible_rows + 2).min(total);
            let mut grid = cosmic::iced::widget::keyed_column(Vec::<(usize, Ui<'_>)>::new());
            if first > 0 {
                grid = grid.push(usize::MAX, Space::new().height(first as f32 * 108.));
            }
            let apps_iter = launcher
                .rows
                .chunks(columns)
                .enumerate()
                .skip(first)
                .take(end - first);
            for (row_index, apps) in apps_iter {
                let mut tiles = row![].spacing(12);
                let app_iter = apps.iter().enumerate();
                for (column_index, app) in app_iter {
                    let icon_size = if app.subtitle == "Application" {
                        44
                    } else {
                        32
                    };
                    let glyph: Ui<'_> = match &app.icon {
                        Some(handle) => cosmic::widget::icon::icon(handle.clone())
                            .size(icon_size)
                            .opacity(opacity)
                            .into(),
                        None => bar_icon(ferese_theme::icons::APPS, icon_size, accent).into(),
                    };
                    let label = container(
                        self.render
                            .text(&app.title)
                            .font(medium)
                            .size(14)
                            .class(theme::Text::Color(text)),
                    )
                    .center_x(Length::Fill)
                    .height(20)
                    .clip(true);
                    let mut content = column![container(glyph).center_x(Length::Fill), label]
                        .spacing(4)
                        .align_x(cosmic::iced::Alignment::Center);
                    if app.subtitle != "Application" {
                        content = content.push(
                            container(
                                self.render
                                    .text(&app.subtitle)
                                    .size(10)
                                    .class(theme::Text::Color(muted)),
                            )
                            .height(16)
                            .clip(true),
                        );
                    }
                    tiles = tiles.push(
                        button::custom(content)
                            .width(Length::Fill)
                            .height(100)
                            .padding(12)
                            .class(button_style(
                                if !launcher.selection_missing
                                    && launcher.selected == row_index * columns + column_index
                                {
                                    palette.accent.scale_alpha(opacity)
                                } else {
                                    Color::TRANSPARENT
                                },
                                self.config.theme.high_contrast,
                            ))
                            .selected(
                                !launcher.selection_missing
                                    && launcher.selected == row_index * columns + column_index,
                            )
                            .name(format!("{}, {}", app.title, app.subtitle))
                            .on_press_maybe(
                                (!launcher.activating)
                                    .then(|| message(Message::ActivateRow(id, app.id.clone()))),
                            ),
                    );
                }
                for _ in apps.len()..columns {
                    tiles = tiles.push(Space::new().width(Length::Fill));
                }
                grid = grid.push(
                    row_index,
                    container(tiles).height(if row_index + 1 == total { 100 } else { 108 }),
                );
            }
            if end < total {
                grid = grid.push(
                    usize::MAX - 1,
                    Space::new().height((total - end) as f32 * 108. - 8.),
                );
            }
            if launcher.rows.is_empty() {
                grid = grid.push(
                    usize::MAX - 2,
                    container(
                        self.render
                            .text(if launcher.running {
                                "Loading apps…"
                            } else {
                                "No apps"
                            })
                            .size(14)
                            .class(theme::Text::Color(muted)),
                    )
                    .height(100),
                );
            }
            let height =
                (launcher.rows.len().div_ceil(columns).clamp(1, visible_rows) as f32 * 108. - 8.)
                    .min(launcher.viewport.1.saturating_sub(80) as f32);
            body = body.push(
                container(
                    cosmic::iced::widget::scrollable(grid)
                        .id(launcher.apps_scroll_id.clone())
                        .on_scroll(move |viewport| {
                            message(Message::AppsScrolled(
                                id,
                                (viewport.absolute_offset().y / 108.) as usize,
                            ))
                        })
                        .height(height),
                )
                .padding([0, 16]),
            );
        } else if launcher.query.is_empty() && launcher.category == Some(Category::Windows) {
            let capacity = launcher.grid_columns().min(3);
            let preview_height = launcher.viewport.1.saturating_sub(120).min(84) as f32;
            let mut cards = row![].spacing(12);
            let result_iter = launcher
                .rows
                .iter()
                .enumerate()
                .skip(launcher.selected.saturating_sub(capacity - 1))
                .take(capacity);
            for (index, result) in result_iter {
                let Target::Window(id) = result.target else {
                    continue;
                };
                let window = self.snapshot.windows.iter().find(|w| w.id == id);
                let workspace = window.and_then(|w| {
                    self.snapshot
                        .workspaces
                        .iter()
                        .find(|ws| ws.id == w.workspace)
                });
                let thumbnail: Ui<'_> = match launcher.thumbnails.images.get(&id) {
                    Some(handle) => image(handle.clone())
                        .width(Length::Fill)
                        .height(preview_height)
                        .content_fit(ContentFit::Contain)
                        .opacity(opacity)
                        .into(),
                    None => container(bar_icon(ferese_theme::icons::WINDOW, 28, muted))
                        .center_x(Length::Fill)
                        .center_y(preview_height)
                        .into(),
                };
                let badge = container(
                    self.render
                        .text(workspace.map_or("?", |w| w.name.as_str()))
                        .size(12)
                        .class(theme::Text::Color(text)),
                )
                .padding([3, 6])
                .class(fill(palette.sidebar.scale_alpha(opacity), 7.));
                let preview = container(stack![thumbnail, container(badge).padding(6)])
                    .clip(true)
                    .class(fill(palette.card.scale_alpha(opacity), 15.));
                let label = self
                    .render
                    .text(window.map_or(result.title.as_str(), |w| {
                        launcher
                            .apps
                            .as_ref()
                            .and_then(|apps| apps.name(&w.app_id))
                            .unwrap_or(if w.app_id.is_empty() {
                                &w.title
                            } else {
                                &w.app_id
                            })
                    }))
                    .size(14)
                    .class(theme::Text::Color(text));
                let selected = !launcher.selection_missing && index == launcher.selected;
                let card = container(column![preview, label].spacing(8))
                    .padding(6)
                    .class(theme::Container::custom(move |_| container::Style {
                        background: selected
                            .then_some(palette.accent.scale_alpha(0.14 * opacity).into()),
                        border: Border {
                            radius: 20.into(),
                            color: palette.accent.scale_alpha(opacity),
                            width: if selected {
                                if high_contrast { 2. } else { 1. }
                            } else {
                                0.
                            },
                            shape: Shape::Continuous,
                            ..Default::default()
                        },
                        ..Default::default()
                    }));
                cards = cards.push(
                    button::custom(card)
                        .width(Length::Fill)
                        .padding(0)
                        .class(button_style(
                            Color::TRANSPARENT,
                            self.config.theme.high_contrast,
                        ))
                        .selected(selected)
                        .name(format!("{}, {}", result.title, result.subtitle))
                        .on_press_maybe((!launcher.activating).then(|| {
                            message(Message::ActivateRow(launcher.id, result.id.clone()))
                        })),
                );
            }
            if launcher.rows.is_empty() {
                cards = cards.push(
                    self.render
                        .text("No open windows")
                        .size(14)
                        .class(theme::Text::Color(muted)),
                );
            }
            body = body.push(container(cards).padding([0, 16]));
        } else if launcher.alternatives
            || (launcher.query.is_empty() && launcher.category.is_some())
        {
            let capacity = launcher.list_rows();
            let start = launcher.selected.saturating_sub(capacity - 1);
            let mut rows = column![].spacing(0);
            let result_iter = launcher.rows.iter().enumerate().skip(start).take(capacity);
            for (index, result) in result_iter {
                let symbol = match result.category() {
                    Some(Category::Windows) => ferese_theme::icons::WINDOW,
                    Some(Category::Files) => ferese_theme::icons::FOLDER,
                    Some(Category::Apps) => ferese_theme::icons::APPS,
                    None => ferese_theme::icons::morph::CONTROLS,
                };
                let tone = accent;
                let result_icon: Ui<'_> = match &result.icon {
                    Some(handle) => cosmic::widget::icon::icon(handle.clone())
                        .size(24)
                        .opacity(opacity)
                        .into(),
                    None => bar_icon(symbol, 20, tone).into(),
                };
                let glyph = container(result_icon)
                    .center_x(36)
                    .center_y(36)
                    .class(fill(tone.scale_alpha(0.1), 12.));
                let labels = column![
                    self.render
                        .text(&result.title)
                        .font(medium)
                        .size(15)
                        .class(theme::Text::Color(text)),
                    self.render
                        .text(&result.subtitle)
                        .size(12.5)
                        .class(theme::Text::Color(muted))
                ]
                .spacing(3)
                .width(Length::Fill);
                let mut content = row![glyph, labels]
                    .spacing(12)
                    .align_y(cosmic::iced::Alignment::Center);
                if !launcher.selection_missing && index == launcher.selected {
                    content = content.push(
                        self.render
                            .text(if result.requires_alt() {
                                "Alt+Enter"
                            } else {
                                "Enter"
                            })
                            .size(12)
                            .class(theme::Text::Color(muted)),
                    );
                }
                rows = rows.push(
                    button::custom(
                        container(content)
                            .height(60)
                            .clip(true)
                            .align_y(cosmic::iced::Alignment::Center),
                    )
                    .width(Length::Fill)
                    .padding([0, 12])
                    .class(button_radius(
                        if !launcher.selection_missing && index == launcher.selected {
                            palette.accent.scale_alpha(opacity)
                        } else {
                            Color::TRANSPARENT
                        },
                        18.,
                        self.config.theme.high_contrast,
                    ))
                    .selected(!launcher.selection_missing && index == launcher.selected)
                    .name(format!("{}, {}", result.title, result.subtitle))
                    .on_press_maybe(
                        (!launcher.activating)
                            .then(|| message(Message::ActivateRow(launcher.id, result.id.clone()))),
                    ),
                );
            }
            if launcher.rows.is_empty() {
                rows = rows.push(
                    container(
                        self.render
                            .text(launcher.status_message().unwrap_or(if launcher.running {
                                "Searching…"
                            } else {
                                "No matches"
                            }))
                            .size(13)
                            .class(theme::Text::Color(muted)),
                    )
                    .height(60)
                    .padding(16),
                );
            }
            body = body.push(container(rows).padding([0, 12]));
        }
        let mut background = color_with_opacity(self.config.theme.surface_popover, opacity);
        if self.config.theme.high_contrast || self.config.theme.reduce_transparency {
            background.a = opacity;
        }
        let compositor_material = launcher.effects.is_some();
        let panel = container(body)
            .id("ferese-blur-card")
            .width(
                launcher
                    .viewport
                    .0
                    .min(if launcher.apps_browse() { 720 } else { 560 }) as f32,
            )
            .height(sample.height.current as f32)
            .clip(true)
            .class(theme::Container::custom(move |_| container::Style {
                background: (!compositor_material).then_some(background.into()),
                text_color: Some(text),
                border: Border {
                    color: muted.scale_alpha(0.16),
                    radius: 28.into(),
                    shape: Shape::Continuous,
                    ..Default::default()
                },
                ..Default::default()
            }));
        let panel = container(motion::scaled(
            opaque(panel),
            0.97 + 0.03 * opacity,
            launcher.regions.clone(),
        ))
        .width(Length::Fill)
        .align_x(cosmic::iced::Alignment::Center);
        let dismiss = message(Message::Dismiss(id));
        let backdrop = mouse_area(Space::new().width(Length::Fill).height(Length::Fill))
            .on_press(dismiss.clone())
            .on_right_press(dismiss.clone())
            .on_middle_press(dismiss);
        stack![backdrop, panel].into()
    }
}
