use super::{
    Alignment, App, Element, Field, Kind, Length, Message, Page, button, column, container, row, schema, scrollable,
    text_input, visuals, widget,
};

pub(super) const SIDEBAR_BREAKPOINT: f32 = 960.;

impl App {
    pub(super) fn page_view(&self) -> Element<'_, Message> {
        cosmic::iced::widget::responsive(move |size| self.page_layout(size.width < SIDEBAR_BREAKPOINT, size.height))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn page_layout(&self, compact: bool, height: f32) -> Element<'_, Message> {
        let palette = visuals::Palette::from_resolved(&self.resolved.presented);
        let mut sidebar = column([])
            .spacing(3)
            .push(
                widget::mouse_area(
                    row([])
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .push(visuals::brand_icon())
                        .push(
                            self.label("Ferese", 16.)
                                .class(cosmic::theme::Text::Color(palette.text)),
                        )
                        .width(Length::Fill),
                )
                .on_press(Message::DragWindow)
                .interaction(cosmic::iced::mouse::Interaction::Grab),
            )
            .push(widget::Space::new().height(14))
            .push(
                text_input("Search settings", self.search.clone())
                    .id(widget::Id::new("settings-search"))
                    .on_input(Message::Search)
                    .font(self.font)
                    .style(visuals::input_style(palette))
                    .size(12),
            )
            .push(widget::Space::new().height(8));
        if compact {
            sidebar = sidebar.push(self.settings_button(
                "Close navigation",
                "M6 6l12 12 M6 18L18 6",
                Some(Message::Sidebar(false)),
                false,
            ));
        }
        let mut navigation = column([]).spacing(16);
        for (title, pages) in Page::NAVIGATION {
            let mut section = column([]).spacing(3).push(
                container(self.label(title, 11.).class(cosmic::theme::Text::Color(palette.muted))).padding([0, 8]),
            );
            for &page in pages {
                let selected = self.page.navigation_page() == page;
                section = section.push(
                    button::custom(
                        row([])
                            .spacing(8)
                            .align_y(Alignment::Center)
                            .push(visuals::icon(
                                page,
                                if selected { palette.accent } else { palette.muted },
                            ))
                            .push(self.label(page.title(), 13.)),
                    )
                    .width(Length::Fill)
                    .padding([8, 8])
                    .class(visuals::navigation_style(palette, selected))
                    .name(page.title())
                    .on_press(Message::Page(if selected { self.page } else { page })),
                );
            }
            navigation = navigation.push(section);
        }
        let sidebar = container(sidebar.push(scrollable(navigation).height(Length::Fill)))
            .id("settings-sidebar")
            .width(if compact { 244 } else { 204 })
            .height(Length::Fill)
            .padding([20, 10])
            .class(visuals::surface(palette.sidebar, 0.));
        let mut heading = row([]).align_y(Alignment::Center).spacing(12).push(
            column([])
                .spacing(3)
                .push(self.label(
                    if self.search.is_empty() {
                        self.page.navigation_page().title()
                    } else {
                        "Search"
                    },
                    20.,
                ))
                .push(
                    self.label(
                        if self.search.is_empty() {
                            self.page.subtitle()
                        } else {
                            "Find a setting for your desktop."
                        },
                        12.,
                    )
                    .class(cosmic::theme::Text::Color(palette.muted)),
                )
                .width(Length::Fill),
        );
        if self.page == Page::Appearance && self.search.is_empty() {
            let automatic = self.draft.string("theme.mode", "dark") == "auto";
            heading = heading.push(
                row([])
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .push(self.label("Auto", 12.))
                    .push(
                        ferese_theme::controls::switch(automatic, palette)
                            .name(if automatic {
                                "Auto appearance: on"
                            } else {
                                "Auto appearance: off"
                            })
                            .on_press(Message::AutoAppearance(!automatic)),
                    ),
            );
            let scheduled = self.draft.string("theme.schedule.source", "schedule") == "schedule";
            let source = if scheduled { "Schedule" } else { "System" };
            let label = if automatic {
                format!(
                    "{source} · {}",
                    if self.resolved.theme.appearance == ferese_config::theme::Appearance::Light {
                        "Light"
                    } else {
                        "Dark"
                    },
                )
            } else {
                source.to_owned()
            };
            let explanation = if scheduled {
                format!(
                    "Auto uses Light from {} and Dark from {} ({})",
                    self.draft.string("theme.schedule.light_at", "07:00"),
                    self.draft.string("theme.schedule.dark_at", "19:00"),
                    self.draft.string("theme.schedule.timezone", "system"),
                )
            } else {
                "Auto follows the GTK/GNOME appearance preference".to_owned()
            };
            heading = heading.push(widget::tooltip(
                self.settings_button(
                    &label,
                    "M12 3a9 9 0 1 0 0 18a9 9 0 0 0 0-18 M12 7v5l3 2",
                    Some(Message::AutoDetails(!self.auto_details)),
                    self.auto_details,
                ),
                self.label(explanation, 12.),
                widget::tooltip::Position::Bottom,
            ));
        }

        let mut body = column([]).spacing(6);

        if !self.search.is_empty() {
            let pages = || {
                Page::NAVIGATION
                    .into_iter()
                    .flat_map(|(_, pages)| pages.iter().copied())
            };
            if !pages().any(|p| p.matches(&self.search)) {
                body = body.push(self.note("No matching settings. Try wallpaper, keyboard, or motion."));
            }
            for page in pages().filter(|p| p.matches(&self.search)) {
                let destination = page.search_destination(&self.search);
                body = body.push(
                    button::custom(
                        column([])
                            .spacing(3)
                            .push(self.label(page.title(), 16.))
                            .push(self.label(destination.subtitle(), 12.)),
                    )
                    .width(Length::Fill)
                    .padding(10)
                    .class(visuals::button_style(palette, false))
                    .on_press(Message::Page(destination)),
                );
            }
        } else {
            if self.page == Page::Windows {
                body = body.push(visuals::preview_resolved(&self.draft, &self.resolved.presented));
            }
            if self.page == Page::Appearance {
                let split = visuals::split(&self.draft);
                if split {
                    body = body.push(self.theme_gallery(Some(ferese_config::theme::Appearance::Light)));
                    body = body.push(self.theme_gallery(Some(ferese_config::theme::Appearance::Dark)));
                } else {
                    body = body.push(self.theme_gallery(None));
                }
                body = body.push(
                    widget::checkbox(split)
                        .label("Use different themes for light and dark")
                        .text_size(12)
                        .font(self.font)
                        .on_toggle(Message::SplitThemes),
                );
                if let Some(note) = &self.resolved.fallback_note {
                    body = body.push(self.note(note));
                }
            }

            if self.page == Page::Wallpaper {
                body = body.push(self.wallpaper_controls());
            }
            if self.page == Page::Bar && self.panel_page != crate::panel_controls::PanelPage::Arrange {
                body = body.push(self.panel_controls());
            }
            let fields = if self.page == Page::Bar {
                Vec::new()
            } else if self.page == Page::Wallpaper {
                self.wallpaper_fields()
            } else {
                schema::fields(self.page)
            };

            if !fields.is_empty() {
                let mut group = column([]).spacing(1);
                if self.page == Page::Appearance && self.auto_details {
                    group = group.push(self.auto_controls());
                }
                for field in fields {
                    if self.page == Page::Appearance
                        && (field.path == "theme.mode"
                            || field.path.starts_with("theme.schedule.")
                            || field.path.ends_with(".file"))
                    {
                        continue;
                    }
                    group = group.push(self.field(field));
                }
                if self.page == Page::Appearance {
                    group = group.push(
                        button::custom(
                            row([])
                                .spacing(8)
                                .align_y(Alignment::Center)
                                .push(self.label("Advanced customization", 13.))
                                .push(visuals::action_icon(
                                    if self.advanced_theme {
                                        "M6 15l6-6 6 6"
                                    } else {
                                        "M6 9l6 6 6-6"
                                    },
                                    palette.muted,
                                )),
                        )
                        .name(if self.advanced_theme {
                            "Collapse advanced customization"
                        } else {
                            "Expand advanced customization"
                        })
                        .padding([7, 10])
                        .width(Length::Fill)
                        .class(visuals::button_style(palette, false))
                        .on_press(Message::AdvancedTheme(!self.advanced_theme)),
                    );
                    if self.advanced_theme {
                        group = group.push(self.theme_file_picker());
                    }
                }
                body = body.push(container(group).padding(4).class(visuals::surface(palette.card, 14.)));
            }

            match self.page {
                Page::Connections => body = body.push(self.connections_view()),
                Page::LockScreen => {
                    body = body.push(self.note("Uses your wallpaper, shell colors, font and corner radius from Appearance. Changes apply when the locker or preview opens."));
                    body = body.push(self.settings_button(
                        "Preview lock screen",
                        "M7 10V7a5 5 0 0 1 10 0v3 M5 10h14v11H5z M12 14v3",
                        Some(Message::PreviewLock),
                        false,
                    ));
                    body = body.push(self.note("The preview is an ordinary window and does not lock your session. Automatic locking is configured separately in Login items."));
                }
                Page::Desktop => {
                    let can_change_list = !self.saving && !self.note_editors.values().any(|e| e.dirty);
                    body = body.push(self.label("Sticky notes", 14.));
                    body = body.push(self.note("Edit here; notes save after you pause typing. Desktop cards stay behind windows and are click-through."));
                    for index in 0..self.draft.records("desktop_widgets.notes") {
                        let id = self.draft.string(&format!("desktop_widgets.notes.{index}.id"), "note");
                        let mut group = column([]).spacing(8);

                        for field in schema::note_fields(index) {
                            group = group.push(self.field(field));
                        }

                        if let Some(editor) = self.note_editors.get(&id) {
                            let id = id.clone();
                            group = group.push(
                                widget::TextEditor::new(&editor.content)
                                    .height(160)
                                    .font(self.font)
                                    .size(13.)
                                    .on_action(move |action| Message::NoteAction(id.clone(), action)),
                            );
                        }
                        group = group.push(self.settings_icon_button(
                            "Remove note",
                            "M4 6h16 M9 6V3h6v3 M6 6l1 15h10l1-15 M10 10v7 M14 10v7",
                            can_change_list.then_some(Message::Remove("desktop_widgets.notes".into(), index)),
                        ));
                        body = body.push(
                            container(group)
                                .padding([7, 10])
                                .class(visuals::surface(palette.card, 14.)),
                        );
                    }
                    body = body.push(
                        self.settings_button(
                            "Add note",
                            "M12 5v14 M5 12h14",
                            (can_change_list && self.draft.records("desktop_widgets.notes") < 32)
                                .then_some(Message::AddNote),
                            false,
                        ),
                    );
                }
                Page::Startup => {
                    body = body.push(self.note("Login items update live. Disabling or removing an item stops the session-owned process; enabling one starts it."));

                    for index in 0..self.draft.records("autostart") {
                        let prefix = format!("autostart.{index}");
                        let group = column([])
                            .spacing(1)
                            .push(self.field(Field::new(
                                format!("{prefix}.command"),
                                "Program",
                                "A program and its arguments, not a shell script.",
                                Kind::Text {
                                    default: "",
                                    argv: true,
                                },
                            )))
                            .push(self.field(Field::new(
                                format!("{prefix}.enabled"),
                                "Open at login",
                                "",
                                Kind::Toggle(true),
                            )))
                            .push(self.field(Field::new(
                                format!("{prefix}.restart"),
                                "Restart if it exits",
                                "Managed by the Ferese session.",
                                Kind::Toggle(true),
                            )))
                            .push(
                                container(self.settings_icon_button(
                                    "Remove login item",
                                    "M4 6h16 M9 6V3h6v3 M6 6l1 15h10l1-15 M10 10v7 M14 10v7",
                                    (!self.saving).then_some(Message::Remove("autostart".into(), index)),
                                ))
                                .padding([7, 10]),
                            );
                        body = body.push(container(group).padding(4).class(visuals::surface(palette.card, 14.)));
                    }
                    body = body.push(
                        row([])
                            .spacing(8)
                            .push(
                                text_input("Program and arguments", self.new_command.clone())
                                    .font(self.font)
                                    .size(12)
                                    .padding([4, 8])
                                    .style(visuals::input_style(palette))
                                    .on_input(Message::NewCommand),
                            )
                            .push(self.settings_button(
                                "Add login item",
                                "M12 5v14 M5 12h14",
                                (!self.saving && !self.new_command.trim().is_empty()).then_some(Message::AddCommand),
                                false,
                            )),
                    );
                }
                Page::Shortcuts => {
                    body = body.push(self.note("Keyboard and swipe bindings share the same actions. Use Swipe3Up, Swipe3Down, Swipe3Left, or Swipe3Right as a shortcut (also supports 4 or 5 fingers). Actions include toggle-overview, spawn, focus, move, workspace-next, workspace-previous, workspace-back-and-forth, and none. Leave Argument empty for actions such as toggle-overview or none."));
                    let mut gestures = row([]).spacing(6);
                    for (keys, label) in [
                        ("Swipe3Up", "Customize swipe up"),
                        ("Swipe3Down", "Customize swipe down"),
                        ("Swipe3Left", "Customize swipe left"),
                        ("Swipe3Right", "Customize swipe right"),
                    ] {
                        let exists = (0..self.draft.records("bindings")).any(|index| {
                            self.draft
                                .string(&format!("bindings.{index}.keys"), "")
                                .eq_ignore_ascii_case(keys)
                        });

                        if !exists {
                            gestures = gestures.push(self.settings_icon_button(
                                label,
                                match keys {
                                    "Swipe3Up" => "M12 20V4 M5 11l7-7 7 7",
                                    "Swipe3Down" => "M12 4v16 M5 13l7 7 7-7",
                                    "Swipe3Left" => "M20 12H4 M11 5l-7 7 7 7",
                                    _ => "M4 12h16 M13 5l7 7-7 7",
                                },
                                (!self.saving).then_some(Message::AddSwipe(keys)),
                            ));
                        }
                    }

                    body = body.push(gestures);

                    for index in 0..self.draft.records("bindings") {
                        let content: Element<'static, Message> = if self.visible_rows.contains(&("bindings", index)) {
                            let prefix = format!("bindings.{index}");
                            let group = column([])
                                .spacing(1)
                                .push(self.field(schema::text(
                                    format!("{prefix}.keys"),
                                    "Shortcut",
                                    "Keys or swipe, such as Super+Return",
                                    "",
                                )))
                                .push(self.field(schema::text(
                                    format!("{prefix}.action"),
                                    "Action",
                                    "Action name, such as spawn or focus",
                                    "",
                                )))
                                .push(self.field(schema::text(
                                    format!("{prefix}.argument"),
                                    "Argument",
                                    "Command, direction, or action argument",
                                    "",
                                )));
                            container(group)
                                .padding(4)
                                .height(164)
                                .class(visuals::surface(palette.card, 14.))
                                .into()
                        } else {
                            widget::Space::new().height(164).width(Length::Fill).into()
                        };

                        body = body.push(
                            cosmic::iced::widget::sensor(content)
                                .key(("bindings", index))
                                .anticipate(200)
                                .on_show(move |_| Message::RowVisibility("bindings", index, true))
                                .on_hide(Message::RowVisibility("bindings", index, false)),
                        );
                    }
                    if self.draft.records("bindings") == 0 {
                        body = body.push(self.note("You are using the built-in shortcuts. Add custom bindings in config.kdl; they will appear here after Reload."));
                    }
                }
                Page::Displays => body = body.push(self.displays_view()),
                _ => {}
            }
        }

        let mut page_heading = row([]).spacing(12).align_y(Alignment::Center);
        if compact {
            page_heading = page_heading.push(
                container(self.settings_icon_button(
                    "Show settings navigation",
                    "M3 5h18v14H3z M9 5v14",
                    Some(Message::Sidebar(true)),
                ))
                .id("settings-navigation-toggle"),
            );
        }
        page_heading = page_heading.push(
            widget::mouse_area(heading.width(Length::Fill))
                .on_press(Message::DragWindow)
                .interaction(cosmic::iced::mouse::Interaction::Grab),
        );
        let mut content = column([])
            .spacing(if self.page == Page::Bar { 16 } else { 8 })
            .push(page_heading);
        if self.search.is_empty() && !self.page.subpages().is_empty() {
            content = content.push(
                container(
                    row(self.page.subpages().iter().map(|&(page, title)| {
                        button::custom(self.label(title, 13.))
                            .name(format!("{}: {title}", self.page.navigation_page().title()))
                            .padding([7, 12])
                            .class(visuals::panel_button(palette, self.page == page))
                            .on_press(Message::Page(page))
                            .into()
                    }))
                    .spacing(2),
                )
                .padding(3)
                .class(visuals::surface(palette.sidebar, 9.)),
            );
        }
        if self.page == Page::Bar && self.search.is_empty() {
            if self.panel_page == crate::panel_controls::PanelPage::Arrange {
                content = content.push(self.panel_navigation());
            }
            content = content.push(self.panel_preview());
        }

        if let Some(error) = &self.error {
            content = content.push(
                container(self.label(error.clone(), 12.))
                    .padding([7, 10])
                    .width(Length::Fill)
                    .class(visuals::surface(palette.error, 10.)),
            );
        }

        if self.page == Page::Bar
            && self.search.is_empty()
            && self.panel_page == crate::panel_controls::PanelPage::Arrange
        {
            content = content.push(self.panel_workspace());
        } else {
            content = content.push(
                scrollable(
                    container(cosmic::iced::widget::keyed_column([(self.page as usize, body.into())]))
                        .padding(cosmic::iced::Padding {
                            top: 0.,
                            right: 12.,
                            bottom: 0.,
                            left: 2.,
                        })
                        .width(Length::Fill),
                )
                .id(widget::Id::new("settings-content"))
                .direction(cosmic::iced::widget::scrollable::Direction::Vertical(
                    cosmic::iced::widget::scrollable::Scrollbar::new()
                        .width(10)
                        .scroller_width(3),
                ))
                .height(Length::Fill),
            );
        }
        let footer = row([])
            .spacing(4)
            .align_y(Alignment::Center)
            .push(widget::tooltip(
                visuals::action_icon(
                    if self.saving {
                        "M12 3v9l5 3 M21 12a9 9 0 1 1-9-9"
                    } else {
                        "M9 12l2 2 4-4 M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0"
                    },
                    palette.muted,
                ),
                self.label(if self.saving { "Saving…" } else { &self.status }, 11.),
                widget::tooltip::Position::Top,
            ))
            .push(widget::Space::new().width(Length::Fill))
            .push(widget::tooltip(
                button::custom(visuals::action_icon(
                    "M3 10h8 M3 10V3 M3 10c3-7 17-6 17 3a7 7 0 0 1-7 7",
                    if self.undo.is_some() && !self.saving {
                        palette.text
                    } else {
                        palette.muted
                    },
                ))
                .padding(4)
                .on_press_maybe((self.undo.is_some() && !self.saving).then_some(Message::Undo)),
                self.label("Undo last change", 11.),
                widget::tooltip::Position::Top,
            ))
            .push(widget::tooltip(
                button::custom(visuals::action_icon(
                    "M21 3v6h-6 M3 21v-6h6 M3 9a9 9 0 0 1 15-6l3 6 M21 15a9 9 0 0 1-15 6l-3-6",
                    palette.text,
                ))
                .padding(4)
                .on_press_maybe((!self.saving).then_some(Message::Reload)),
                self.label("Reload configuration", 11.),
                widget::tooltip::Position::Top,
            ));
        let footer: Element<'_, Message> = if self.page == Page::Connections {
            self.note("Connections are managed by the system. Passwords are not saved in your Ferese config.")
        } else if self.page == Page::Bar {
            container(
                row([])
                    .spacing(12)
                    .align_y(Alignment::Center)
                    .push(self.note(if self.saving { "Saving…" } else { &self.status }))
                    .push(widget::Space::new().width(Length::Fill))
                    .push(self.settings_button(
                        "Undo",
                        "M3 10h8 M3 10V3 M3 10c3-7 17-6 17 3a7 7 0 0 1-7 7",
                        (self.undo.is_some() && !self.saving).then_some(Message::Undo),
                        false,
                    ))
                    .push(self.settings_button(
                        "Reload configuration",
                        "M21 3v6h-6 M3 21v-6h6 M3 9a9 9 0 0 1 15-6l3 6 M21 15a9 9 0 0 1-15 6l-3-6",
                        (!self.saving).then_some(Message::Reload),
                        false,
                    )),
            )
            .padding([10, 0])
            .into()
        } else {
            footer.into()
        };
        let mut drawer = None;
        let sidebar: Element<'_, Message> = if compact {
            if self.sidebar_open {
                drawer = Some(sidebar);
            }
            widget::Space::new().width(0).into()
        } else {
            sidebar.into()
        };
        let view = row([]).push(sidebar).push(
            container(
                container(column([]).push(content.height(Length::Fill)).push(footer).spacing(6))
                    .padding(cosmic::iced::Padding {
                        top: 20.,
                        right: 20.,
                        bottom: 8.,
                        left: 20.,
                    })
                    .max_width(if self.page == Page::Bar { 1120 } else { 840 })
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .width(Length::Fill)
            .center_x(Length::Fill)
            .height(Length::Fill)
            .id("settings-page")
            .class(visuals::surface(palette.background, 0.)),
        );
        // Keep the page mounted and full-width while compact navigation is open.
        let mut view = widget::popover(view).modal(true);
        if let Some(sidebar) = drawer {
            let dismiss = widget::mouse_area(
                container(widget::Space::new().width(Length::Fill).height(Length::Fill))
                    .class(visuals::surface(cosmic::iced::Color::BLACK.scale_alpha(0.20), 0.)),
            )
            .on_press(Message::Sidebar(false));
            view = view
                .position(widget::popover::Position::Point(cosmic::iced::Point::ORIGIN))
                .popup(row([]).push(sidebar).push(dismiss).width(Length::Fill).height(height));
        }
        let view: Element<'_, Message> = view.into();
        if self.profile_pages {
            let page = self.page;
            cosmic::iced::widget::sensor(view)
                .key((page as usize, self.search.is_empty()))
                .on_show(move |_| Message::PagePresented(page))
                .into()
        } else {
            view
        }
    }
}
