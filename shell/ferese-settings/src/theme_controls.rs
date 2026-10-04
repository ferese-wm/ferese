use super::{
    Alignment, App, Edit, Element, Kind, Length, Message, Page, PathBuf, button, column, container, gallery, row,
    schema, set, text_input, visuals, widget,
};

#[derive(Clone, Default)]
pub(super) struct FamilySections {
    built_in: std::sync::Arc<Vec<usize>>,
    custom: std::sync::Arc<Vec<usize>>,
}

impl FamilySections {
    pub(super) fn new(families: &[ferese_config::families::Family], snapshot: &crate::store::Snapshot) -> Self {
        let (custom, built_in) = (0..families.len()).partition(|&index| {
            snapshot
                .item(&format!("theme.custom_themes.{}", families[index].id))
                .is_some()
        });
        Self {
            built_in: std::sync::Arc::new(built_in),
            custom: std::sync::Arc::new(custom),
        }
    }

    fn indices(&self, custom: bool) -> &std::sync::Arc<Vec<usize>> {
        if custom { &self.custom } else { &self.built_in }
    }

    pub(super) fn position(&self, index: usize) -> Option<(bool, usize)> {
        [false, true].into_iter().find_map(|custom| {
            self.indices(custom)
                .iter()
                .position(|&family| family == index)
                .map(|position| (custom, position))
        })
    }
}

impl App {
    pub(super) fn theme_file_picker(&self) -> Element<'static, Message> {
        let palette = visuals::Palette::from_resolved(&self.resolved.presented);
        let path = ["theme.file", "theme.light.file", "theme.dark.file"][self.theme_file_target];
        let file = self.draft.string(path, "");
        let filename = if file.is_empty() {
            "No override file".to_owned()
        } else {
            PathBuf::from(&file)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        };
        let labels = column([])
            .spacing(2)
            .width(Length::Fill)
            .push(self.label("Theme overrides", 13.))
            .push(widget::tooltip(
                self.label(filename, 11.)
                    .class(cosmic::theme::Text::Color(palette.muted)),
                self.label(
                    if file.is_empty() {
                        "Choose a partial KDL theme file".to_owned()
                    } else {
                        file.clone()
                    },
                    11.,
                ),
                widget::tooltip::Position::Bottom,
            ));
        let mut scopes = row([]).spacing(2);
        for (index, label) in ["Shared", "Light", "Dark"].into_iter().enumerate() {
            scopes = scopes.push(
                button::custom(self.label(label, 12.))
                    .name(format!("Theme file: {label}"))
                    .padding([4, 8])
                    .class(visuals::button_style(palette, index == self.theme_file_target))
                    .on_press(Message::ThemeFileTarget(index)),
            );
        }
        let mut controls = row([])
            .spacing(6)
            .align_y(Alignment::Center)
            .push(
                container(scopes)
                    .padding(3)
                    .class(visuals::surface(palette.sidebar, 10.)),
            )
            .push(self.settings_button(
                if file.is_empty() { "Choose…" } else { "Replace…" },
                "M3 7V5h6l2 2h10v13H3z",
                Some(Message::PickThemeFile(path)),
                false,
            ));
        if !file.is_empty() {
            controls = controls.push(widget::tooltip(
                button::custom(visuals::action_icon("M6 6l12 12M6 18L18 6", palette.muted))
                    .padding(4)
                    .on_press(Message::Change(Edit::Unset(path.into()))),
                self.label("Remove override", 11.),
                widget::tooltip::Position::Bottom,
            ));
        }
        container(
            row([])
                .spacing(6)
                .align_y(Alignment::Center)
                .push(labels)
                .push(controls),
        )
        .padding([7, 10])
        .width(Length::Fill)
        .class(visuals::surface(palette.card, 0.))
        .into()
    }

    pub(super) fn auto_controls(&self) -> Element<'static, Message> {
        let palette = visuals::Palette::from_resolved(&self.resolved.presented);
        let scheduled = self.draft.string("theme.schedule.source", "schedule") == "schedule";
        let mut sources = row([]).spacing(4);
        for (source, label) in [("system", "System"), ("schedule", "Schedule")] {
            sources = sources.push(self.settings_button(
                label,
                if source == "schedule" {
                    "M12 3a9 9 0 1 0 0 18a9 9 0 0 0 0-18 M12 7v5l3 2"
                } else {
                    "M3 4h18v13H3z M12 17v4 M8 21h8"
                },
                Some(Message::Change(set("theme.schedule.source", source))),
                scheduled == (source == "schedule"),
            ));
        }
        let description = if scheduled {
            "Switch between light and dark at set times."
        } else {
            "Follow the GTK/GNOME appearance preference."
        };
        let mut options = column([]).spacing(6).push(
            row([])
                .spacing(6)
                .align_y(Alignment::Center)
                .push(
                    column([])
                        .spacing(2)
                        .width(Length::Fill)
                        .push(self.label("Automatic appearance", 13.))
                        .push(self.note(description)),
                )
                .push(sources),
        );
        if scheduled {
            let mut times = row([]).spacing(6).align_y(Alignment::Center);
            for field in schema::fields(Page::Appearance)
                .into_iter()
                .filter(|field| field.path.starts_with("theme.schedule."))
            {
                let path = field.path.clone();
                let Kind::Text { default, .. } = &field.kind else {
                    continue;
                };
                let value = self
                    .inputs
                    .get(&path)
                    .cloned()
                    .unwrap_or_else(|| self.draft.string(&path, default));
                let submit = field.clone();
                let input = text_input(*default, value)
                    .font(self.font)
                    .padding([4, 8])
                    .size(12)
                    .style(visuals::input_style(palette))
                    .on_input(move |value| Message::Draft(path.clone(), value))
                    .on_submit(move |_| Message::Commit(submit.clone()))
                    .on_unfocus(Message::Commit(field.clone()))
                    .width(if field.path.ends_with("timezone") { 150 } else { 75 });
                times = times.push(
                    row([])
                        .spacing(6)
                        .align_y(Alignment::Center)
                        .push(self.label(field.label, 12.))
                        .push(input),
                );
            }
            options = options.push(times);
        }
        container(options)
            .padding([7, 10])
            .width(Length::Fill)
            .class(visuals::surface(palette.card, 0.))
            .into()
    }

    pub(super) fn theme_gallery(&self, appearance: Option<ferese_config::theme::Appearance>) -> Element<'_, Message> {
        use ferese_config::theme::Appearance::{Dark, Light};
        let mut heading = row([]).align_y(Alignment::Center).spacing(6).push(
            self.label(
                match appearance {
                    Some(Light) => "Light theme",
                    Some(Dark) => "Dark theme",
                    None => "Theme",
                },
                14.,
            )
            .width(Length::Fill),
        );
        if appearance != Some(Dark) && self.undo.is_some() {
            heading = heading.push(self.settings_icon_button(
                "Undo",
                "M4 10h8 M4 10V3 M4 10c3-7 16-6 16 3a7 7 0 0 1-7 7",
                (!self.saving).then_some(Message::Undo),
            ));
        }
        let mut custom_heading = row([])
            .align_y(Alignment::Center)
            .spacing(6)
            .push(self.label("Custom themes", 14.).width(Length::Fill));
        if appearance != Some(Dark) {
            custom_heading = custom_heading.push(self.settings_button(
                "Import…",
                "M3 7V5h6l2 2h10v13H3z M12 10v7 M9 14l3 3 3-3",
                Some(Message::ImportTheme),
                false,
            ));
        }

        let mut sections = column([]).spacing(12).push(
            column([])
                .spacing(6)
                .push(heading)
                .push(self.theme_gallery_grid(appearance, false)),
        );
        if appearance != Some(Dark) || !self.family_sections.custom.is_empty() {
            sections = sections.push(
                column([])
                    .spacing(6)
                    .push(custom_heading)
                    .push(self.theme_gallery_grid(appearance, true)),
            );
        }

        sections.into()
    }

    fn theme_gallery_grid(
        &self,
        appearance: Option<ferese_config::theme::Appearance>,
        custom: bool,
    ) -> Element<'_, Message> {
        let mut rows = column([]).spacing(6);
        let list = crate::navigation::gallery_list(appearance, custom);
        for chunk in 0..self.family_sections.indices(custom).len().div_ceil(3) {
            let content = widget::responsive(move |size| {
                // Three borderless previews, with names overlaid inside each image.
                let height = ((size.width - 12.) / 3.).max(0.) * 126. / 200.;
                let content: Element<'_, Message> = if self.visible_rows.contains(&(list, chunk)) {
                    self.theme_gallery_row(chunk, appearance, custom)
                } else {
                    widget::Space::new().height(height).width(Length::Fill).into()
                };
                cosmic::iced::widget::sensor(
                    container(content)
                        .height(height)
                        .id(crate::navigation::gallery_row_id(list, chunk)),
                )
                .key((list, chunk))
                .anticipate(200)
                .on_show(move |_| Message::RowVisibility(list, chunk, true))
                .on_hide(Message::RowVisibility(list, chunk, false))
                .into()
            })
            .height(Length::Shrink);
            rows = rows.push(content);
        }

        gallery::group(
            container(rows).width(Length::Fill),
            cosmic::iced::advanced::widget::Id::new(format!("{list}-gallery")),
            if custom {
                "Custom themes"
            } else {
                match appearance {
                    Some(ferese_config::theme::Appearance::Light) => "Light theme",
                    Some(ferese_config::theme::Appearance::Dark) => "Dark theme",
                    None => "Theme",
                }
            },
        )
    }

    pub(super) fn gallery_position(
        &self,
        id: &str,
        appearance: Option<ferese_config::theme::Appearance>,
    ) -> Option<(&'static str, usize)> {
        let index = self.family_ids.iter().position(|family| family == id)?;
        let (custom, position) = self.family_sections.position(index)?;
        Some((crate::navigation::gallery_list(appearance, custom), position / 3))
    }

    fn theme_gallery_row(
        &self,
        chunk: usize,
        appearance: Option<ferese_config::theme::Appearance>,
        custom: bool,
    ) -> Element<'static, Message> {
        let palette = visuals::Palette::from_resolved(&self.resolved.presented);
        let selected_id = visuals::family_selection(&self.draft, appearance);
        let indices = self.family_sections.indices(custom);
        let families = &indices[chunk * 3..indices.len().min(chunk * 3 + 3)];
        let has_selection = indices
            .iter()
            .any(|&index| self.resolved.families[index].id == selected_id);
        let ids = &self.family_ids;
        let mut tiles = row([]).spacing(6);
        for (offset, &family_index) in families.iter().enumerate() {
            let family = &self.resolved.families[family_index];
            let selected = family.id == selected_id;
            let index = chunk * 3 + offset;
            let id = family.id.clone();
            let active = selected
                .then_some(self.resolved.theme.appearance)
                .filter(|a| appearance.is_none_or(|variant| variant == *a));
            let tile_id = gallery_id(&id, appearance);
            let ids = ids.clone();
            let indices = indices.clone();
            tiles = tiles.push(gallery::tile(
                family,
                gallery::TileOptions {
                    variant: appearance,
                    active,
                    selected,
                    tab_stop: selected || (!has_selection && index == 0),
                    palette,
                    font: self.font,
                    id: tile_id,
                },
                Message::Family(id, appearance),
                move |key| {
                    gallery::neighbor(index, indices.len(), key)
                        .map(|index| Message::Family(ids[indices[index]].clone(), appearance))
                },
            ));
        }
        for _ in families.len()..3 {
            tiles = tiles.push(widget::Space::new().width(Length::Fill));
        }

        tiles.into()
    }
}

pub(super) fn gallery_id(
    id: &str,
    appearance: Option<ferese_config::theme::Appearance>,
) -> cosmic::iced::advanced::widget::Id {
    cosmic::iced::advanced::widget::Id::new(format!("theme-{appearance:?}-{id}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_use_registered_imports_not_id_prefixes_or_catalog_position() {
        let snapshot = crate::store::Snapshot::parse(
            "theme { custom-themes { sunset { file \"sunset.kdl\"; }; dracula { file \"mine.kdl\"; }; }; }".into(),
        )
        .unwrap();
        let mut families = ferese_config::families::builtins();
        let mut imported = families[0].clone();
        imported.id = "sunset".into();
        families.insert(1, imported.clone());
        imported.id = "dracula".into();
        families.push(imported);
        let mut legacy = families[0].clone();
        legacy.id = "monokai".into();
        families.push(legacy);
        let sections = FamilySections::new(&families, &snapshot);
        assert_eq!(sections.custom.as_slice(), &[1, 7]);
        assert_eq!(sections.built_in.as_slice(), &[0, 2, 3, 4, 5, 6, 8]);
        assert_eq!(sections.position(7), Some((true, 1)));
        assert_eq!(sections.position(8), Some((false, 6)));
    }
}
