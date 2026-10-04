use super::{
    Anchor, Background, Border, Color, Element, FereseShell, IcedMargin, IcedOutput, KeyboardInteractivity, Layer,
    Length, Limits, Message, NoteDrag, SctkLayerSurfaceSettings, Task, Zoned, alignment, button, color, config,
    configured_font, container, destroy_layer_surface, format_bar_time, motion, note_store, row, set_anchor,
    set_margin, stacked_clock_digits, text, theme, window,
};
use cosmic::iced::border::Shape as BorderShape;

impl FereseShell {
    pub(super) fn flush_note_changes(&mut self) -> Task<Message> {
        if self.note_saving || self.note_pending.is_empty() {
            return Task::none();
        }

        let Some(path) = config::config_path() else {
            self.note_error = Some("Configuration path unavailable.".into());
            return Task::none();
        };
        self.note_saving = true;
        self.note_inflight = std::mem::take(&mut self.note_pending);
        let edits = self.note_inflight.clone();
        cosmic::task::future(async move {
            let (send, receive) = cosmic::iced::futures::channel::oneshot::channel();
            std::thread::spawn(move || {
                let _ = send.send(note_store::save(&path, &edits));
            });
            Message::NotesStored(
                receive
                    .await
                    .unwrap_or_else(|_| Err("Note save worker stopped.".into())),
            )
        })
    }

    pub(super) fn set_note_text(&mut self, id: &str, value: String) -> Task<Message> {
        if let Some(note) = self.config.desktop_widgets.notes.iter_mut().find(|note| note.id == id)
            && (note.text != value || self.note_error.is_some())
        {
            note.text = value.clone();
            self.note_pending.push(note_store::Edit::Text(id.to_owned(), value));
        }
        self.flush_note_changes()
    }

    pub(super) fn finish_note_edit(&mut self) -> Task<Message> {
        if let Some(editor) = self.note_editor.take() {
            self.set_note_text(&editor.id, editor.content.text())
        } else {
            self.flush_note_changes()
        }
    }

    pub(super) fn begin_note_drag(&mut self, id: String, surface: window::Id) -> Task<Message> {
        self.begin_widget_drag(Some(id), surface)
    }

    pub(super) fn begin_widget_drag(&mut self, id: Option<String>, surface: window::Id) -> Task<Message> {
        if self.note_drag.is_some() {
            return Task::none();
        }
        let Some(entry) = self
            .outputs
            .iter()
            .find(|entry| entry.clock == Some(surface) || entry.notes.iter().any(|(_, window)| *window == surface))
        else {
            return Task::none();
        };
        let Some(size) = entry.size else {
            return Task::none();
        };
        let (origin, dimensions) = if let Some(id) = &id {
            let Some(note) = self.config.desktop_widgets.notes.iter().find(|note| &note.id == id) else {
                return Task::none();
            };
            (note_origin(note, size), (note.width, note.height))
        } else {
            let clock = &self.config.desktop_widgets.clock;
            (
                widget_origin(
                    clock.anchor,
                    clock.margin_x,
                    clock.margin_y,
                    (clock.width, clock.height),
                    size,
                ),
                (clock.width, clock.height),
            )
        };
        let output = entry.output.clone();
        let mut obstacles = Vec::new();
        let clock = &self.config.desktop_widgets.clock;

        if id.is_some() && clock.on_output(entry.name.as_deref()) {
            let position = widget_origin(
                clock.anchor,
                clock.margin_x,
                clock.margin_y,
                (clock.width, clock.height),
                size,
            );
            obstacles.push(cosmic::iced::Rectangle::new(
                position,
                cosmic::iced::Size::new(clock.width as f32, clock.height as f32),
            ));
        }

        for note in &self.config.desktop_widgets.notes {
            if Some(&note.id) != id.as_ref() && note.on_output(entry.name.as_deref()) {
                obstacles.push(cosmic::iced::Rectangle::new(
                    note_origin(note, size),
                    cosmic::iced::Size::new(note.width as f32, note.height as f32),
                ));
            }
        }

        let overlay = window::Id::unique();
        self.note_drag = Some(NoteDrag {
            id,
            source: surface,
            overlay,
            origin,
            position: origin,
            start: self.note_pointer.get(&surface).copied().unwrap_or_default(),
            output_size: size,
            obstacles,
        });
        let action = cosmic::surface::action::app_layer_shell::<Self>(
            |_| Default::default(),
            move |_| SctkLayerSurfaceSettings {
                id: overlay,
                layer: Layer::Overlay,
                keyboard_interactivity: KeyboardInteractivity::None,
                input_zone: Some(Vec::new()),
                anchor: Anchor::TOP | Anchor::LEFT,
                margin: IcedMargin {
                    top: origin.y.round() as i32,
                    left: origin.x.round() as i32,
                    ..Default::default()
                },
                output: IcedOutput::Output(output.clone()),
                namespace: "ferese-shell-note-drag".into(),
                exclusive_zone: -1,
                size: Some((Some(dimensions.0), Some(dimensions.1))),
                size_limits: Limits::NONE,
            },
            Some(Box::new(Self::view_note_drag)),
        );

        Task::batch([
            self.finish_note_edit(),
            cosmic::task::message(cosmic::Action::Surface(action)),
        ])
    }

    pub(super) fn view_note_drag(&self) -> Element<'_, cosmic::Action<Message>> {
        let Some(drag) = &self.note_drag else {
            return text("").into();
        };

        if let Some(id) = &drag.id {
            self.view_note_content(id, drag.source)
        } else {
            self.view_clock_content()
        }
    }

    pub(super) fn finish_note_drag(&mut self, save: bool) -> Task<Message> {
        let Some(drag) = self.note_drag.take() else {
            return Task::none();
        };
        let mut tasks = vec![destroy_layer_surface(drag.overlay)];

        if save {
            if drag.id.is_none() {
                let clock = &mut self.config.desktop_widgets.clock;
                clock.anchor = ferese_config::desktop::Anchor::TopLeft;
                clock.margin_x = drag.position.x.round() as i32;
                clock.margin_y = drag.position.y.round() as i32;
                for entry in &self.outputs {
                    if let Some(id) = entry.clock {
                        tasks.push(set_anchor(id, Anchor::TOP | Anchor::LEFT));
                        tasks.push(set_margin(id, clock.margin_y, 0, 0, clock.margin_x));
                    }
                }
                self.note_pending
                    .push(note_store::Edit::ClockPosition(clock.margin_x, clock.margin_y));
            }

            if let Some(note) = self
                .config
                .desktop_widgets
                .notes
                .iter_mut()
                .find(|note| Some(&note.id) == drag.id.as_ref())
            {
                note.anchor = ferese_config::desktop::Anchor::TopLeft;
                note.margin_x = drag.position.x.round() as i32;
                note.margin_y = drag.position.y.round() as i32;

                for entry in &self.outputs {
                    if let Some((_, id)) = entry.notes.iter().find(|(id, _)| Some(id) == drag.id.as_ref()) {
                        tasks.push(set_anchor(*id, Anchor::TOP | Anchor::LEFT));
                        tasks.push(set_margin(*id, note.margin_y, 0, 0, note.margin_x));
                    }
                }

                self.note_pending.push(note_store::Edit::Position(
                    note.id.clone(),
                    note.margin_x,
                    note.margin_y,
                ));
            }

            tasks.push(self.flush_note_changes());
        }
        Task::batch(tasks)
    }

    pub(super) fn rebuild_notes(&mut self, reset: bool) -> Task<Message> {
        let mut tasks = Vec::new();

        for entry in &mut self.outputs {
            if reset {
                for (_, id) in entry.notes.drain(..) {
                    self.note_pointer.remove(&id);
                    tasks.push(destroy_layer_surface(id));
                }
            }

            for note in &self.config.desktop_widgets.notes {
                if !note.on_output(entry.name.as_deref()) || entry.notes.iter().any(|(id, _)| id == &note.id) {
                    continue;
                }

                let id = window::Id::unique();
                entry.notes.push((note.id.clone(), id));
                let note = note.clone();
                let note_id = note.id.clone();
                let output = entry.output.clone();
                let action = cosmic::surface::action::app_layer_shell::<Self>(
                    |_| Default::default(),
                    move |_| {
                        let (anchor, margin) = widget_placement(note.anchor, note.margin_x, note.margin_y);
                        SctkLayerSurfaceSettings {
                            id,
                            layer: Layer::Bottom,
                            keyboard_interactivity: if note.interactive {
                                KeyboardInteractivity::OnDemand
                            } else {
                                KeyboardInteractivity::None
                            },
                            input_zone: if note.interactive { None } else { Some(Vec::new()) },
                            anchor,
                            margin,
                            output: IcedOutput::Output(output.clone()),
                            namespace: "ferese-shell-sticky-note".into(),
                            exclusive_zone: -1,
                            size: Some((Some(note.width), Some(note.height))),
                            size_limits: Limits::NONE,
                        }
                    },
                    Some(Box::new(move |app| app.view_note(&note_id, id))),
                );

                tasks.push(cosmic::task::message(cosmic::Action::Surface(action)));
            }
        }

        Task::batch(tasks)
    }

    pub(super) fn view_note(&self, id: &str, surface: window::Id) -> Element<'_, cosmic::Action<Message>> {
        if self
            .note_drag
            .as_ref()
            .is_some_and(|drag| drag.id.as_deref() == Some(id))
        {
            return container(text("")).width(Length::Fill).height(Length::Fill).into();
        }

        self.view_note_content(id, surface)
    }

    pub(super) fn view_note_content(&self, id: &str, surface: window::Id) -> Element<'_, cosmic::Action<Message>> {
        let Some(note) = self.config.desktop_widgets.notes.iter().find(|note| note.id == id) else {
            return text("").into();
        };
        let alignment = match note.alignment {
            ferese_config::desktop::Alignment::Left => alignment::Horizontal::Left,
            ferese_config::desktop::Alignment::Center => alignment::Horizontal::Center,
            ferese_config::desktop::Alignment::Right => alignment::Horizontal::Right,
        };
        let font = configured_font(
            note.font_family
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .or(self.config.font_family.as_deref()),
        );
        let mut foreground = color(
            note.color
                .as_deref()
                .and_then(config::parse_color)
                .unwrap_or(self.config.theme.text_primary),
        );

        foreground.a *= note.opacity;

        let background = if note.background.as_deref() == Some("") {
            None
        } else {
            let mut tint = color(
                note.background
                    .as_deref()
                    .and_then(config::parse_color)
                    .unwrap_or(self.config.theme.surface_base),
            );
            tint.a *= note.opacity;
            Some(Background::Color(tint))
        };
        let mut body = cosmic::widget::column([]).spacing(note.gap).align_x(alignment);

        if note.interactive {
            let title = ferese_theme::text(if note.title.is_empty() { "⋮⋮" } else { &note.title }, font)
                .size(note.title_size)
                .class(theme::Text::Color(foreground));
            let header = cosmic::widget::mouse_area(container(title).width(Length::Fill))
                .interaction(cosmic::iced::mouse::Interaction::Grab)
                .on_press(cosmic::Action::App(Message::BeginNoteDrag(id.to_owned(), surface)));
            let editing = self.note_editor.as_ref().is_some_and(|editor| editor.id == id);

            body = body.push(
                row([]).push(header).push(motion::button(
                    button::custom(text(if editing { "Done" } else { "Edit" }))
                        .padding([4, 8])
                        .on_press(cosmic::Action::App(if editing {
                            Message::FinishNoteEdit
                        } else {
                            Message::BeginNoteEdit(id.to_owned())
                        })),
                    foreground,
                    false,
                    1.0,
                )),
            );
        } else if !note.title.is_empty() {
            body = body.push(
                ferese_theme::text(
                    note.title.clone(),
                    cosmic::font::Font {
                        weight: cosmic::iced::font::Weight::Semibold,
                        ..font
                    },
                )
                .size(note.title_size)
                .width(Length::Fill)
                .align_x(alignment)
                .class(theme::Text::Color(foreground)),
            );
        }

        if let Some(editor) = &self.note_editor
            && editor.id == id
        {
            let id = id.to_owned();
            let editor_radius = ferese_theme::inner_radius(self.config.theme.material_radius, note.padding);
            body = body.push(
                cosmic::widget::TextEditor::new(&editor.content)
                    .style(move |theme, status| {
                        use cosmic::iced::widget::text_editor::Catalog;
                        let mut style = theme.style(&<cosmic::Theme as Catalog>::default(), status);
                        style.border.radius = editor_radius.into();
                        style
                    })
                    .height(Length::Fill)
                    .font(font)
                    .size(note.text_size)
                    .on_action(move |action| cosmic::Action::App(Message::NoteAction(id.clone(), action))),
            );
        } else {
            body = body.push(
                ferese_theme::text(note.text.clone(), font)
                    .size(note.text_size)
                    .width(Length::Fill)
                    .align_x(alignment)
                    .class(theme::Text::Color(foreground)),
            );
        }

        if let Some(error) = &self.note_error {
            body = body.push(text(error.clone()).size(10).class(theme::Text::Color(foreground)));
        }

        let radius = self.config.theme.material_radius;

        container(body)
            .padding(note.padding)
            .width(Length::Fill)
            .height(Length::Fill)
            .class(theme::Container::custom(move |_| container::Style {
                background,
                border: Border {
                    shape: BorderShape::Continuous,
                    radius: radius.into(),
                    ..Default::default()
                },
                ..Default::default()
            }))
            .into()
    }

    pub(super) fn rebuild_clocks(&mut self, reset: bool) -> Task<Message> {
        self.desktop_clock = self
            .config
            .desktop_widgets
            .clock
            .labels(&Zoned::now())
            .unwrap_or_default();
        let mut tasks = Vec::new();

        for entry in &mut self.outputs {
            if !reset && entry.clock.is_some() {
                continue;
            }

            if let Some(id) = entry.clock.take() {
                self.note_pointer.remove(&id);
                tasks.push(destroy_layer_surface(id));
            }

            let clock = self.config.desktop_widgets.clock.clone();

            if !clock.on_output(entry.name.as_deref()) {
                continue;
            }

            let id = window::Id::unique();
            entry.clock = Some(id);
            let output = entry.output.clone();
            let action = cosmic::surface::action::app_layer_shell::<Self>(
                |_| Default::default(),
                move |_| {
                    let (anchor, margin) = clock_placement(&clock);
                    SctkLayerSurfaceSettings {
                        id,
                        layer: Layer::Bottom,
                        keyboard_interactivity: KeyboardInteractivity::OnDemand,
                        input_zone: None,
                        anchor,
                        margin,
                        output: IcedOutput::Output(output.clone()),
                        namespace: "ferese-shell-desktop-clock".into(),
                        exclusive_zone: -1,
                        size: Some((Some(clock.width), Some(clock.height))),
                        size_limits: Limits::NONE,
                    }
                },
                Some(Box::new(move |app| app.view_desktop_clock(id))),
            );

            tasks.push(cosmic::task::message(cosmic::Action::Surface(action)));
        }

        Task::batch(tasks)
    }

    pub(super) fn view_desktop_clock(&self, id: window::Id) -> Element<'_, cosmic::Action<Message>> {
        if self
            .note_drag
            .as_ref()
            .is_some_and(|drag| drag.id.is_none() && drag.source == id)
        {
            return container(text("")).width(Length::Fill).height(Length::Fill).into();
        }

        cosmic::widget::mouse_area(self.view_clock_content())
            .on_press(cosmic::Action::App(Message::BeginClockDrag(id)))
            .interaction(cosmic::iced::mouse::Interaction::Grab)
            .into()
    }

    pub(super) fn view_clock_content(&self) -> Element<'_, cosmic::Action<Message>> {
        use ferese_config::desktop::Alignment as ClockAlignment;
        let clock = &self.config.desktop_widgets.clock;
        let alignment = match clock.alignment {
            ClockAlignment::Left => alignment::Horizontal::Left,
            ClockAlignment::Center => alignment::Horizontal::Center,
            ClockAlignment::Right => alignment::Horizontal::Right,
        };
        let pixel = clock.style == ferese_config::desktop::ClockStyle::Pixel;
        let custom_font = clock.font_family.as_deref().filter(|family| !family.trim().is_empty());
        let mut font = configured_font(if pixel {
            custom_font.or(Some("Cantarell"))
        } else {
            custom_font.or(self.config.font_family.as_deref())
        });
        if pixel && custom_font.is_none() {
            font.weight = cosmic::iced::font::Weight::Black;
        } else if clock.bold {
            font.weight = cosmic::iced::font::Weight::Bold;
        }
        let date_font = configured_font(custom_font.or(self.config.font_family.as_deref()));
        let tint = |custom: &Option<String>, fallback| {
            let mut tint = color(custom.as_deref().and_then(config::parse_color).unwrap_or(fallback));
            tint.a *= clock.opacity;
            tint
        };
        let foreground = tint(&clock.color, self.config.theme.text_primary);
        let accent = color(self.config.theme.accent);
        let custom_color = clock.color.as_deref().and_then(config::parse_color).is_some();
        let digit_colors = [0.8, 0.45].map(|text_mix| {
            if custom_color {
                return foreground;
            }
            Color {
                r: foreground.r * text_mix + accent.r * (1.0 - text_mix),
                g: foreground.g * text_mix + accent.g * (1.0 - text_mix),
                b: foreground.b * text_mix + accent.b * (1.0 - text_mix),
                a: foreground.a,
            }
        });
        let mut labels = cosmic::widget::column([]).spacing(clock.gap).align_x(alignment);

        if pixel && clock.show_date {
            labels = labels.push(
                ferese_theme::text(self.desktop_clock.1.clone(), date_font)
                    .size(clock.date_size)
                    .class(theme::Text::Color(tint(
                        &clock.date_color,
                        self.config.theme.text_primary,
                    ))),
            );
        }

        if let Some(digits) = pixel.then(|| stacked_clock_digits(&self.desktop_clock.0)).flatten() {
            let available_height = (clock.height as f32
                - clock.padding * 2.0
                - if clock.show_date {
                    clock.date_size * 1.3 + clock.gap
                } else {
                    0.0
                })
            .max(16.0);
            let size = clock
                .time_size
                .min(available_height / 1.6)
                .min((clock.width as f32 - clock.padding * 2.0).max(16.0) / 1.5);
            let mut stack = cosmic::widget::column([]).spacing(0).align_x(alignment);

            for (line, digits) in digits.into_iter().enumerate() {
                let mut digit_row = row([]).spacing(0);

                for (position, digit) in digits.chars().enumerate() {
                    digit_row = digit_row.push(
                        ferese_theme::text(digit.to_string(), font)
                            .size(size)
                            .line_height(cosmic::iced::widget::text::LineHeight::Relative(0.8))
                            .class(theme::Text::Color(digit_colors[(line + position) % 2])),
                    );
                }

                stack = stack.push(digit_row);
            }

            labels = labels.push(stack);
        } else {
            let characters = self.desktop_clock.0.chars().count().max(1) as f32;
            let available_width = (clock.width as f32 - clock.padding * 2.0).max(1.0);
            let available_height = (clock.height as f32
                - clock.padding * 2.0
                - if clock.show_date {
                    clock.date_size * 1.3 + clock.gap
                } else {
                    0.0
                })
            .max(1.0);
            let size = clock
                .time_size
                .min(available_width / (characters * 0.75))
                .min(available_height / 1.3);

            labels = labels.push(
                ferese_theme::text(self.desktop_clock.0.clone(), font)
                    .size(size)
                    .width(Length::Fill)
                    .align_x(alignment)
                    .class(theme::Text::Color(foreground)),
            );
        }

        if !pixel && clock.show_date {
            labels = labels.push(
                ferese_theme::text(self.desktop_clock.1.clone(), font)
                    .size(clock.date_size)
                    .width(Length::Fill)
                    .align_x(alignment)
                    .class(theme::Text::Color(tint(
                        &clock.date_color,
                        self.config.theme.text_muted,
                    ))),
            );
        }
        let background = clock.background.as_deref().and_then(config::parse_color).map(|rgba| {
            let mut tint = color(rgba);
            tint.a *= clock.opacity;
            Background::Color(tint)
        });
        let radius = self.config.theme.material_radius;

        container(labels)
            .padding(clock.padding)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(alignment)
            .center_y(Length::Fill)
            .class(theme::Container::custom(move |_| container::Style {
                background,
                border: Border {
                    shape: BorderShape::Continuous,
                    radius: radius.into(),
                    ..Default::default()
                },
                ..Default::default()
            }))
            .into()
    }
}

/// Sweep each axis, allowing edge sliding without tunnelling through a widget
/// when pointer events skip ahead. Coordinates match integer layer margins.
pub(super) fn avoid_widget_overlap(
    previous: cosmic::iced::Point,
    requested: cosmic::iced::Point,
    size: (u32, u32),
    obstacles: &[cosmic::iced::Rectangle],
) -> cosmic::iced::Point {
    let mut position = previous;
    let (width, height) = (size.0 as f32, size.1 as f32);
    let mut x = requested.x.round();
    for obstacle in obstacles {
        if position.y < obstacle.y + obstacle.height && position.y + height > obstacle.y {
            if x > position.x && position.x + width <= obstacle.x {
                x = x.min((obstacle.x - width).floor());
            } else if x < position.x && position.x >= obstacle.x + obstacle.width {
                x = x.max((obstacle.x + obstacle.width).ceil());
            }
        }
    }
    position.x = x;
    let mut y = requested.y.round();
    for obstacle in obstacles {
        if position.x < obstacle.x + obstacle.width && position.x + width > obstacle.x {
            if y > position.y && position.y + height <= obstacle.y {
                y = y.min((obstacle.y - height).floor());
            } else if y < position.y && position.y >= obstacle.y + obstacle.height {
                y = y.max((obstacle.y + obstacle.height).ceil());
            }
        }
    }
    position.y = y;
    position
}

pub(super) fn clamp_note_position(
    position: cosmic::iced::Point,
    output: (i32, i32),
    size: (u32, u32),
) -> cosmic::iced::Point {
    cosmic::iced::Point::new(
        position.x.clamp(0., (output.0 as f32 - size.0 as f32).clamp(0., 8192.)),
        position.y.clamp(0., (output.1 as f32 - size.1 as f32).clamp(0., 8192.)),
    )
}

pub(super) fn note_origin(note: &ferese_config::desktop::StickyNote, output: (i32, i32)) -> cosmic::iced::Point {
    widget_origin(
        note.anchor,
        note.margin_x,
        note.margin_y,
        (note.width, note.height),
        output,
    )
}

pub(super) fn widget_origin(
    anchor: ferese_config::desktop::Anchor,
    margin_x: i32,
    margin_y: i32,
    size: (u32, u32),
    output: (i32, i32),
) -> cosmic::iced::Point {
    use ferese_config::desktop::Anchor as A;
    let x = match anchor {
        A::TopLeft | A::CenterLeft | A::BottomLeft => margin_x as f32,
        A::TopRight | A::CenterRight | A::BottomRight => output.0 as f32 - size.0 as f32 - margin_x as f32,
        _ => (output.0 as f32 - size.0 as f32) / 2.,
    };
    let y = match anchor {
        A::TopLeft | A::TopCenter | A::TopRight => margin_y as f32,
        A::BottomLeft | A::BottomCenter | A::BottomRight => output.1 as f32 - size.1 as f32 - margin_y as f32,
        _ => (output.1 as f32 - size.1 as f32) / 2.,
    };
    clamp_note_position(cosmic::iced::Point::new(x, y), output, size)
}

pub(super) fn clock_placement(clock: &ferese_config::desktop::Clock) -> (Anchor, IcedMargin) {
    widget_placement(clock.anchor, clock.margin_x, clock.margin_y)
}

pub(super) fn widget_placement(
    position: ferese_config::desktop::Anchor,
    margin_x: i32,
    margin_y: i32,
) -> (Anchor, IcedMargin) {
    use ferese_config::desktop::Anchor as Position;
    let horizontal = match position {
        Position::TopLeft | Position::CenterLeft | Position::BottomLeft => Anchor::LEFT,
        Position::TopRight | Position::CenterRight | Position::BottomRight => Anchor::RIGHT,
        _ => Anchor::empty(),
    };
    let vertical = match position {
        Position::TopLeft | Position::TopCenter | Position::TopRight => Anchor::TOP,
        Position::BottomLeft | Position::BottomCenter | Position::BottomRight => Anchor::BOTTOM,
        _ => Anchor::empty(),
    };
    (
        horizontal | vertical,
        IcedMargin {
            top: if vertical == Anchor::TOP { margin_y } else { 0 },
            bottom: if vertical == Anchor::BOTTOM { margin_y } else { 0 },
            left: if horizontal == Anchor::LEFT { margin_x } else { 0 },
            right: if horizontal == Anchor::RIGHT { margin_x } else { 0 },
        },
    )
}

pub(super) fn current_time() -> String {
    format_bar_time(&Zoned::now())
}
