use std::io::{Read, Write};

use cosmic::app::{Core, Settings, Task};
use cosmic::{ApplicationExt, Element};
use serde::{Deserialize, Serialize};

use crate::capture::Source;
use crate::recording::Audio;

mod backdrop;
mod view;

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
            Self::Displays => ferese_theme::icons::DISPLAY,
            Self::Workspaces => ferese_theme::icons::morph::WORKSPACES,
            Self::Windows => ferese_theme::icons::WINDOW,
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
        let groups: Vec<_> = SourceGroup::available(self).collect();
        let rows = groups
            .iter()
            .map(|group| group.count(self))
            .max()
            .unwrap_or(1);
        (260.
            + rows.min(4) as f32 * 64.
            + if groups.len() > 1 { 44. } else { 0. }
            + if rows > 6 { 44. } else { 0. })
        .clamp(400., 600.)
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

struct Picker {
    core: Core,
    material: Option<ferese_theme_client::material::ModalMaterial>,
    parent: Option<std::sync::Arc<crate::parent::Parent>>,
    prompt: Prompt,
    selected: Vec<usize>,
    group: SourceGroup,
    query: String,
    options_source: Option<usize>,
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
        return Err("Picker request is too large".into());
    }
    let prompt: Prompt = serde_json::from_str(&input)?;

    if prompt.sources.is_empty() || prompt.sources.len() > 272 {
        return Err("No shareable displays".into());
    }

    let (theme, font, background, palette) = appearance();
    let height = prompt.dialog_height();
    cosmic::app::run::<Picker>(
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

impl cosmic::Application for Picker {
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
        match message {
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
                return cosmic::iced::window::run(id, move |window| {
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
        self.view_dialog()
    }
}

impl Picker {
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
        content: impl Into<std::borrow::Cow<'a, str>> + 'a,
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
mod tests;
