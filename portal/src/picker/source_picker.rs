use std::borrow::Cow;
use std::io::{Read, Write};

use cosmic::app::{Core, Settings, Task};
use cosmic::iced::{Alignment, Length};
use cosmic::widget::{button, column, row};
use cosmic::{ApplicationExt, Element, theme, widget};
use ferese_theme::icons;
use serde::{Deserialize, Serialize};

use crate::capture::Source;
use crate::recording::Audio;

use super::{backdrop, content_size};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceGroup {
    Displays,
    Workspaces,
    Windows,
}

impl SourceGroup {
    fn available(prompt: &Prompt) -> impl Iterator<Item = Self> + '_ {
        [Self::Displays, Self::Workspaces, Self::Windows]
            .into_iter()
            .filter(|group| group.count(prompt) > 0)
    }

    fn contains(self, source: &Source) -> bool {
        matches!(
            (self, source.kind()),
            (Self::Displays, crate::capture::SourceKind::Monitor)
                | (Self::Workspaces, crate::capture::SourceKind::Workspace(_))
                | (Self::Windows, crate::capture::SourceKind::Window(_))
        )
    }

    fn count(self, prompt: &Prompt) -> usize {
        prompt
            .sources
            .iter()
            .filter(|source| self.contains(source))
            .count()
    }

    fn label(self) -> &'static str {
        match self {
            Self::Displays => "Displays",
            Self::Workspaces => "Workspaces",
            Self::Windows => "Windows",
        }
    }

    fn icon(self) -> &'static [u8] {
        match self {
            Self::Displays => icons::DISPLAY,
            Self::Workspaces => icons::morph::WORKSPACES,
            Self::Windows => icons::WINDOW,
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Displays => "Everything visible on the selected display is shared.",
            Self::Workspaces => "This workspace keeps sharing when you switch away.",
            Self::Windows => "Only the selected window is captured.",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Prompt {
    pub app: String,
    pub sources: Vec<Source>,
    pub multiple: bool,
    #[serde(default)]
    pub parent: String,
    #[serde(default)]
    pub window_capture: bool,
    #[serde(default)]
    pub persist_mode: u32,
    #[serde(default)]
    pub rememberable: Vec<String>,
    #[serde(default)]
    pub recording: bool,
    #[serde(default)]
    pub workspace_shell: bool,
    #[serde(default)]
    pub workspace_quality: bool,
}

impl Prompt {
    pub(crate) fn allows_shell<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> bool {
        self.allows_workspace_options(self.workspace_shell, names)
    }

    pub(crate) fn allows_quality<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> bool {
        self.allows_workspace_options(self.workspace_quality, names)
    }

    fn allows_workspace_options<'a>(
        &self,
        supported: bool,
        names: impl IntoIterator<Item = &'a str>,
    ) -> bool {
        supported
            && !self.window_capture
            && !self.recording
            && names.into_iter().any(|name| {
                self.sources
                    .iter()
                    .any(|source| source.workspace_id().is_some() && source.name == name)
            })
    }

    fn dialog_height(&self) -> f32 {
        let groups = SourceGroup::available(self).count();
        let rows = SourceGroup::available(self)
            .next()
            .map_or(1, |group| group.count(self));
        // Startup hint; the rendered content supplies the final window size.
        (200.
            + rows.min(4) as f32 * 60.
            + rows.min(4).saturating_sub(1) as f32 * 6.
            + if groups > 1 { 52. } else { 0. }
            + if rows > 6 { 50. } else { 0. })
        .min(600.)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Selection {
    pub names: Vec<String>,
    pub persist_mode: u32,
    #[serde(default)]
    pub audio: Audio,
    #[serde(default)]
    pub region: bool,
    #[serde(default)]
    pub include_shell: bool,
    #[serde(default = "crate::workspace::Quality::legacy_default")]
    pub workspace_quality: crate::workspace::Quality,
}

#[derive(Clone, Debug)]
enum Message {
    ThemeChanged(Box<ferese_ipc::theme::Snapshot>),
    WindowOpened(cosmic::iced::window::Id),
    Attached(crate::parent::Attachment),
    Measure,
    ContentSize(Option<cosmic::iced::Size>),
    MonitorSize(Option<cosmic::iced::Size>),
    Select(usize),
    Group(SourceGroup),
    Search(String),
    OpenOptions(usize),
    CloseOptions,
    Escape,
    Remember,
    Audio(Audio),
    Region,
    IncludeShell,
    WorkspaceQuality(crate::workspace::Quality),
    Share,
    Cancel,
}

struct SourcePicker {
    core: Core,
    material: Option<ferese_theme_client::material::ModalMaterial>,
    parent: Option<std::sync::Arc<crate::parent::Parent>>,
    prompt: Prompt,
    selected: Vec<usize>,
    group: SourceGroup,
    query: String,
    options_source: Option<usize>,
    content_limited: bool,
    max_height: f32,
    last_size: Option<cosmic::iced::Size>,
    remember: bool,
    audio: Audio,
    region: bool,
    include_shell: bool,
    workspace_quality: crate::workspace::Quality,
    background: cosmic::iced::Color,
    palette: ferese_theme::Palette,
    font: cosmic::font::Font,
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    std::io::stdin()
        .take(256 * 1024 + 1)
        .read_to_string(&mut input)?;
    if input.len() > 256 * 1024 {
        return Err("SourcePicker request is too large".into());
    }
    let prompt: Prompt = serde_json::from_str(&input)?;

    if prompt.sources.is_empty() || prompt.sources.len() > 272 {
        return Err("No shareable displays".into());
    }

    let (theme, font, background, palette) = appearance();
    let height = prompt.dialog_height();
    cosmic::app::run::<SourcePicker>(
        Settings::default()
            .size(cosmic::iced::Size::new(560., height))
            .client_decorations(false)
            .transparent(true)
            .theme(theme)
            .default_font(font)
            .default_text_size(14.)
            .is_daemon(false),
        (prompt, background, palette, font),
    )?;

    Ok(())
}

impl cosmic::Application for SourcePicker {
    type Executor = cosmic::executor::Default;
    type Flags = (
        Prompt,
        cosmic::iced::Color,
        ferese_theme::Palette,
        cosmic::font::Font,
    );
    type Message = Message;
    const APP_ID: &'static str = "dev.ferese.ScreenShare";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(
        mut core: Core,
        (prompt, background, palette, font): Self::Flags,
    ) -> (Self, Task<Message>) {
        core.window.show_headerbar = false;
        core.window.border_padding = Some(0);
        core.window.content_container = false;
        core.window.use_template = false;
        // No preselected source: clicking Share must be a deliberate choice.
        let group = SourceGroup::available(&prompt)
            .next()
            .unwrap_or(SourceGroup::Displays);
        let mut app = Self {
            core,
            material: None,
            parent: None,
            prompt,
            selected: Vec::new(),
            group,
            query: String::new(),
            options_source: None,
            content_limited: false,
            max_height: 600.,
            last_size: None,
            remember: false,
            audio: Audio::Off,
            region: false,
            include_shell: false,
            workspace_quality: crate::workspace::Quality::default(),
            background,
            palette,
            font,
        };
        let task = app
            .core
            .main_window_id()
            .map(|id| {
                app.set_window_title(
                    if app.prompt.recording {
                        "Ferese — screen recording".into()
                    } else if app.prompt.window_capture {
                        "Ferese — capture a window".into()
                    } else {
                        "Ferese — share your screen".into()
                    },
                    id,
                )
            })
            .unwrap_or_else(Task::none);

        (app, task)
    }

    fn subscription(&self) -> cosmic::iced::Subscription<Message> {
        cosmic::iced::Subscription::batch([
            ferese_theme_client::service::subscription()
                .map(|snapshot| Message::ThemeChanged(Box::new(snapshot))),
            cosmic::iced::event::listen_with(|event, _, id| match event {
                cosmic::iced::Event::Window(cosmic::iced::window::Event::Opened { .. }) => {
                    Some(Message::WindowOpened(id))
                }
                cosmic::iced::Event::Window(cosmic::iced::window::Event::Resized(_)) => {
                    Some(Message::Measure)
                }
                cosmic::iced::Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed {
                    key:
                        cosmic::iced::keyboard::Key::Named(cosmic::iced::keyboard::key::Named::Escape),
                    ..
                }) => Some(Message::Escape),
                _ => None,
            }),
        ])
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        if !matches!(message, Message::ContentSize(_)) {
            self.content_limited = false;
        }
        match message {
            Message::ContentSize(Some(size)) => {
                if !self.content_limited && size.height > self.max_height + 0.5 {
                    self.content_limited = true;
                    return Task::none();
                }
                let size = cosmic::iced::Size::new(
                    size.width.ceil(),
                    size.height.min(self.max_height).ceil(),
                );
                if self.last_size != Some(size) {
                    self.last_size = Some(size);
                    if let Some(id) = self.core.main_window_id() {
                        return cosmic::iced::window::resize(id, size);
                    }
                }
                return Task::none();
            }
            Message::ContentSize(None) => return Task::none(),
            Message::MonitorSize(Some(size)) if size.height.is_finite() && size.height > 24. => {
                self.max_height = (size.height - 24.).min(600.);
            }
            Message::ThemeChanged(snapshot) => {
                self.palette = ferese_theme::Palette::from_resolved(&snapshot.presented).flat();
                self.font =
                    ferese_theme::font(Some(&snapshot.presented.tokens.typography.font_family));
                self.background = cosmic::iced::Color {
                    a: ferese_theme::material_opacity(&snapshot.presented),
                    ..self.palette.sidebar
                };
                return cosmic::command::set_theme(self.palette.native_theme());
            }
            Message::WindowOpened(id) => {
                let parent = self.prompt.parent.clone();
                let attach = cosmic::iced::window::run(id, move |window| {
                    let attached = crate::parent::Parent::attach(window, &parent)?;
                    if let Some(diagnostic) = attached.diagnostic {
                        eprintln!("ferese portal: {diagnostic}");
                    }
                    Ok((
                        attached.parent.map(std::sync::Arc::new),
                        ferese_theme_client::material::ModalMaterial::attach(window),
                    ))
                })
                .map(|result| cosmic::Action::App(Message::Attached(result)));
                return Task::batch([
                    attach,
                    cosmic::iced::window::monitor_size(id)
                        .map(|size| cosmic::Action::App(Message::MonitorSize(size))),
                ]);
            }
            Message::Attached(Ok((parent, material))) => {
                self.parent = parent;
                self.material = material.ok();
            }
            Message::Attached(Err(error)) => {
                eprintln!("ferese portal: {error}");
                return cosmic::iced::exit();
            }
            Message::Select(index)
                if self.options_source.is_none() && index < self.prompt.sources.len() =>
            {
                if !self.visible_sources().any(|(visible, _)| visible == index) {
                    return Task::none();
                }
                if self.selected.contains(&index) {
                    self.selected.retain(|i| *i != index);
                } else {
                    if self.prompt.multiple && self.selected.len() >= 8 {
                        return Task::none();
                    }
                    if !self.prompt.multiple {
                        self.selected.clear();
                    }
                    self.selected.push(index);
                }
                self.remember &= self.can_remember();
                self.include_shell &= self.can_include_shell();
                if self.selected.is_empty() {
                    self.options_source = None;
                }
            }
            Message::Group(group)
                if self.options_source.is_none()
                    && group.count(&self.prompt) > 0
                    && group != self.group =>
            {
                self.group = group;
                self.query.clear();
                // Single-source consent always belongs to the visible tab.
                if !self.prompt.multiple {
                    self.selected.clear();
                    self.remember = false;
                    self.include_shell = false;
                    self.options_source = None;
                }
            }
            Message::Search(query) if self.options_source.is_none() => self.query = query,
            Message::OpenOptions(index)
                if self.source_has_options(index)
                    && self.visible_sources().any(|(visible, _)| visible == index) =>
            {
                self.options_source = Some(index);
            }
            Message::CloseOptions => self.options_source = None,
            Message::Escape if self.options_source.is_some() => self.options_source = None,
            Message::Remember if self.options_monitor() && self.can_remember() => {
                self.remember = !self.remember
            }
            Message::Audio(audio) if self.options_monitor() && self.prompt.recording => {
                self.audio = audio
            }
            Message::Region if self.options_monitor() && self.prompt.recording => {
                self.region = !self.region
            }
            Message::IncludeShell if self.options_workspace() && self.can_include_shell() => {
                self.include_shell = !self.include_shell
            }
            Message::WorkspaceQuality(quality)
                if self.options_workspace() && self.can_choose_quality() =>
            {
                self.workspace_quality = quality;
            }
            Message::Share if self.options_source.is_none() && !self.selected.is_empty() => {
                let names: Vec<_> = self
                    .selected
                    .iter()
                    .map(|i| &self.prompt.sources[*i].name)
                    .collect();
                if self.prompt.window_capture {
                    println!(
                        "{}",
                        serde_json::to_string(&names)
                            .expect("typed configuration and IPC values must serialize to JSON")
                    );
                } else {
                    let selection = Selection {
                        names: names.into_iter().cloned().collect(),
                        persist_mode: if self.remember && self.can_remember() {
                            self.prompt.persist_mode
                        } else {
                            0
                        },
                        audio: self.audio,
                        region: self.region,
                        include_shell: self.include_shell && self.can_include_shell(),
                        workspace_quality: if self.can_choose_quality() {
                            self.workspace_quality
                        } else {
                            crate::workspace::Quality::Balanced
                        },
                    };
                    println!(
                        "{}",
                        serde_json::to_string(&selection)
                            .expect("typed configuration and IPC values must serialize to JSON")
                    );
                }
                let _ = std::io::stdout().flush();
                return cosmic::iced::exit();
            }
            Message::Cancel | Message::Escape => return cosmic::iced::exit(),
            _ => (),
        }
        Task::none()
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(self.palette.application_style(if self.material.is_some() {
            cosmic::iced::Color::TRANSPARENT
        } else {
            self.background
        }))
    }

    fn view(&self) -> Element<'_, Message> {
        content_size::fit(
            self.view_dialog(),
            self.content_limited.then_some(self.max_height),
            |size| Message::ContentSize(Some(size)),
        )
    }
}

impl SourcePicker {
    fn visible_sources(&self) -> impl Iterator<Item = (usize, &Source)> {
        let query = self.query.trim().to_lowercase();
        self.prompt
            .sources
            .iter()
            .enumerate()
            .filter(move |(_, source)| {
                self.group.contains(source)
                    && (query.is_empty()
                        || source.label.to_lowercase().contains(&query)
                        || source.name.to_lowercase().contains(&query))
            })
    }

    fn source_has_options(&self, index: usize) -> bool {
        self.selected.contains(&index)
            && self.prompt.sources.get(index).is_some_and(|source| {
                (source.workspace_id().is_some()
                    && (self.can_include_shell() || self.can_choose_quality()))
                    || (source.is_monitor() && (self.prompt.recording || self.can_remember()))
            })
    }

    fn options_workspace(&self) -> bool {
        self.options_source
            .and_then(|index| self.prompt.sources.get(index))
            .is_some_and(|source| source.workspace_id().is_some())
    }

    fn options_monitor(&self) -> bool {
        self.options_source
            .and_then(|index| self.prompt.sources.get(index))
            .is_some_and(Source::is_monitor)
    }

    fn can_choose_quality(&self) -> bool {
        self.prompt.allows_quality(
            self.selected
                .iter()
                .map(|index| self.prompt.sources[*index].name.as_str()),
        )
    }

    fn can_include_shell(&self) -> bool {
        self.prompt.allows_shell(
            self.selected
                .iter()
                .map(|index| self.prompt.sources[*index].name.as_str()),
        )
    }

    fn can_remember(&self) -> bool {
        self.prompt.persist_mode > 0
            && !self.selected.is_empty()
            && self.selected.len() <= 16
            && self.selected.iter().all(|index| {
                self.prompt
                    .rememberable
                    .contains(&self.prompt.sources[*index].name)
            })
    }

    fn text<'a>(
        &self,
        content: Cow<'a, str>,
    ) -> cosmic::widget::Text<'a, cosmic::Theme, cosmic::Renderer> {
        ferese_theme::text(content, self.font)
    }
}

pub(crate) fn appearance() -> (
    cosmic::Theme,
    cosmic::font::Font,
    cosmic::iced::Color,
    ferese_theme::Palette,
) {
    let snapshot = ferese_theme_client::service::current();
    let palette = ferese_theme::Palette::from_resolved(&snapshot.presented).flat();
    let family = &snapshot.presented.tokens.typography.font_family;
    (
        palette.native_theme(),
        ferese_theme::font(Some(family)),
        cosmic::iced::Color {
            a: ferese_theme::material_opacity(&snapshot.presented),
            ..palette.sidebar
        },
        palette,
    )
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

impl SourcePicker {
    fn view_dialog(&self) -> Element<'_, Message> {
        let content = self.view_contents();
        if let Some(source) = self
            .options_source
            .and_then(|index| self.prompt.sources.get(index))
        {
            cosmic::widget::popover::popover(backdrop::backdrop(
                content,
                self.options_dialog(source),
            ))
            .modal(true)
            .popup(self.options_dialog(source))
            .into()
        } else {
            content
        }
    }

    fn view_contents(&self) -> Element<'_, Message> {
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
        let close = button::custom(icons::symbolic(icons::CLOSE, 16))
            .class(self.control_style(false))
            .padding(8)
            .name("Close dialog")
            .on_press(Message::Cancel);
        let header = row![
            column![
                self.text(title.into()).size(22),
                self.muted(subtitle.into()).size(13).ellipsize(
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

        let height = if self.content_limited {
            Length::Fill
        } else {
            Length::Shrink
        };
        let mut content = column![header].spacing(18).height(height);
        if SourceGroup::available(&self.prompt).count() > 1 {
            content = content.push(self.source_tabs());
        }
        if self.group.count(&self.prompt) > 6 {
            content = content.push(
                widget::text_input(
                    format!("Search {}", self.group.label().to_lowercase()),
                    &self.query,
                )
                .font(self.font)
                .style(ferese_theme::controls::material_input(
                    self.palette,
                    self.background.a,
                ))
                .on_input(Message::Search),
            );
        }
        content = content.push(self.muted(self.group.description().into()).size(12));
        let empty = self.visible_sources().next().is_none();
        let mut sources = widget::column([]).spacing(6).width(Length::Fill);
        for (index, source) in self.visible_sources() {
            sources = sources.push(self.source_row(index, source));
        }
        let source_list: Element<'_, Message> = if empty {
            widget::container(self.muted("No matching sources".into()).size(14))
                .center_x(Length::Fill)
                .center_y(60)
                .into()
        } else {
            let sources = widget::scrollable(sources)
                .direction(cosmic::iced::widget::scrollable::Direction::Vertical(
                    cosmic::iced::widget::scrollable::Scrollbar::hidden(),
                ))
                .height(height)
                .width(Length::Fill);
            if self.content_limited {
                sources.into()
            } else {
                widget::container(sources).max_height(258).into()
            }
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
            .class(ferese_theme::accent_button())
            .on_press_maybe((!self.selected.is_empty()).then_some(Message::Share));
        let footer = row![
            self.muted(selected.into())
                .size(12)
                .width(Length::Fill)
                .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                    cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1)
                )),
            cancel,
            share,
        ]
        .spacing(12)
        .align_y(Alignment::Center);
        widget::container(content.push(footer))
            .padding(24)
            .width(Length::Fill)
            .height(height)
            .into()
    }

    fn source_tabs(&self) -> Element<'_, Message> {
        let mut tabs = widget::row([]).spacing(4);
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
                    widget::container(self.text(label.into()).size(13))
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
        let icon = widget::container(icons::symbolic(self.group.icon(), 22))
            .center_x(40)
            .center_y(40);
        let title = self
            .text(self.source_title(source).into())
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
            icons::symbolic(icons::CHECK, 18).into()
        } else {
            cosmic::iced::widget::Space::new().width(18).into()
        };
        let select = button::custom(
            row![
                icon,
                column![title, self.muted(metadata.into()).size(12)]
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
                button::custom(icons::symbolic(icons::SETTINGS, 18))
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

    fn options_dialog(&self, source: &Source) -> Element<'_, Message> {
        let title = if self.prompt.recording {
            "Recording options"
        } else {
            "Sharing options"
        };
        let header = row![
            self.text(title.into()).size(20).width(Length::Fill),
            button::custom(icons::symbolic(icons::CLOSE, 16))
                .class(self.control_style(false))
                .name("Close options")
                .padding(7)
                .on_press(Message::CloseOptions)
        ]
        .spacing(12)
        .align_y(Alignment::Center);
        let mut content = column![
            header,
            self.muted(self.source_title(source).into())
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
                self.muted(format!("Applies to all {workspaces} selected workspaces.").into())
                    .size(12),
            );
        }
        content = content.push(row![
            cosmic::iced::widget::Space::new().width(Length::Fill),
            ferese_theme::controls::text_button("Done", self.font, self.palette, true)
                .class(ferese_theme::accent_button())
                .on_press(Message::CloseOptions),
        ]);
        let palette = self.palette;
        widget::container(content)
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
        let mut options = widget::column([]).spacing(16).width(Length::Fill);
        if self.options_workspace() && self.can_choose_quality() {
            let mut choices = widget::row([]).spacing(6);
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
                    self.text("Workspace quality".into()).size(13),
                    choices,
                    self.muted(self.workspace_quality.description().into())
                        .size(12)
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
            let mut audio = widget::column([]).spacing(6);
            for choices in [
                [Audio::Off, Audio::System],
                [Audio::Microphone, Audio::Both],
            ] {
                let mut choices_row = widget::row([]).spacing(6);
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
            options = options.push(column![self.text("Audio".into()).size(13), audio].spacing(6));
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
        widget::container(options).padding([4, 0]).into()
    }

    fn option_switch<'a>(
        &self,
        label: &'a str,
        description: &'a str,
        enabled: bool,
        message: Message,
    ) -> Element<'a, Message> {
        row![
            column![
                self.text(label.into()).size(13),
                self.muted(description.into()).size(12)
            ]
            .spacing(4)
            .width(Length::Fill),
            ferese_theme::controls::switch(enabled, self.palette).on_press(message)
        ]
        .spacing(16)
        .align_y(Alignment::Center)
        .into()
    }

    fn control_style(&self, selected: bool) -> theme::Button {
        ferese_theme::controls::material_button_style(self.palette, selected, self.background.a)
    }

    fn muted<'a>(
        &self,
        value: Cow<'a, str>,
    ) -> cosmic::widget::Text<'a, cosmic::Theme, cosmic::Renderer> {
        self.text(value)
            .class(theme::Text::Color(self.palette.muted))
    }
}
