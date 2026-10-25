use std::io::{Read, Write};

use cosmic::app::{Core, Settings, Task};
use cosmic::iced::{Alignment, Length};
use cosmic::widget::{button, column, container, row, scrollable};
use cosmic::{ApplicationExt, Element};
use ferese_theme::icons::symbolic as glyph;
use serde::{Deserialize, Serialize};

use crate::capture::Source;
use crate::recording::Audio;

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
}

impl Prompt {
    pub(crate) fn allows_shell<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> bool {
        self.workspace_shell
            && !self.window_capture
            && !self.recording
            && names.into_iter().any(|name| {
                self.sources
                    .iter()
                    .any(|source| source.workspace_id().is_some() && source.name == name)
            })
    }

    fn source_list_height(&self) -> f32 {
        let visible = &self.sources[..self.sources.len().min(4)];
        let mut sections = 0;
        let mut previous_kind = None;
        if !self.window_capture {
            for source in visible {
                let kind = source.kind().category();
                if previous_kind != Some(kind) {
                    sections += 1;
                    previous_kind = Some(kind);
                }
            }
        }
        let rows = visible.len();
        (rows * 64 + rows.saturating_sub(1) * 6 + sections * 24) as f32
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
}

#[derive(Clone, Debug)]
enum Message {
    ThemeChanged(Box<ferese_ipc::theme::Snapshot>),
    WindowOpened(cosmic::iced::window::Id),
    Attached(crate::parent::Attachment),
    Select(usize),
    Remember,
    Audio(Audio),
    Region,
    IncludeShell,
    Share,
    Cancel,
}

struct Picker {
    core: Core,
    material: Option<ferese_theme_client::material::ModalMaterial>,
    parent: Option<std::sync::Arc<crate::parent::Parent>>,
    prompt: Prompt,
    selected: Vec<usize>,
    remember: bool,
    audio: Audio,
    region: bool,
    include_shell: bool,
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
    let height = 180.
        + prompt.source_list_height()
        + if prompt.persist_mode > 0 { 48. } else { 0. }
        + if prompt.recording { 190. } else { 0. }
        + if prompt.workspace_shell { 80. } else { 0. };
    cosmic::app::run::<Picker>(
        Settings::default()
            .size(cosmic::iced::Size::new(400., height))
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
        let mut app = Self {
            core,
            material: None,
            parent: None,
            prompt,
            selected: Vec::new(),
            remember: false,
            audio: Audio::Off,
            region: false,
            include_shell: false,
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
            Message::Select(index) if index < self.prompt.sources.len() => {
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
            }
            Message::Remember if self.can_remember() => self.remember = !self.remember,
            Message::Audio(audio) if self.prompt.recording => self.audio = audio,
            Message::Region if self.prompt.recording => self.region = !self.region,
            Message::IncludeShell if self.can_include_shell() => {
                self.include_shell = !self.include_shell
            }
            Message::Share if !self.selected.is_empty() => {
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
            Message::Cancel => return cosmic::iced::exit(),
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
        let app = if self.prompt.app.is_empty() {
            "this application"
        } else {
            &self.prompt.app
        };
        let header = row![
            glyph(ferese_theme::icons::DISPLAY, 24),
            column![
                self.text(if self.prompt.recording {
                    "Record your screen"
                } else if self.prompt.window_capture {
                    "Choose a window"
                } else {
                    "Choose what to share"
                })
                .size(17),
                self.text(if self.prompt.recording {
                    "Saved to your Videos folder".to_owned()
                } else {
                    format!(
                        "{} {app}",
                        if self.prompt.window_capture {
                            "Screenshot for"
                        } else {
                            "Share with"
                        }
                    )
                })
                .size(12),
            ]
            .spacing(2)
            .width(Length::Fill),
        ]
        .spacing(12)
        .align_y(Alignment::Center);
        let mut sources = column([]).spacing(6);
        let mut previous_kind = None;
        for (index, source) in self.prompt.sources.iter().enumerate() {
            let kind = source.kind().category();
            if !self.prompt.window_capture && previous_kind != Some(kind) {
                sources = sources.push(self.text(source.kind().label()).size(12));
                previous_kind = Some(kind);
            }
            let selected = self.selected.contains(&index);
            let mut entry = row![
                glyph(ferese_theme::icons::DISPLAY, 24),
                column![
                    self.text(if self.prompt.window_capture || !source.is_monitor() {
                        &source.label
                    } else {
                        &source.name
                    })
                    .size(14),
                    self.text(format!("{} × {}", source.width, source.height))
                        .size(12)
                ]
                .spacing(3)
                .width(Length::Fill),
            ]
            .spacing(12)
            .align_y(Alignment::Center);
            if selected {
                entry = entry.push(glyph(ferese_theme::icons::CHECK, 20));
            }
            sources = sources.push(
                button::custom(entry)
                    .class(ferese_theme::controls::material_button_style(
                        self.palette,
                        selected,
                        self.background.a,
                    ))
                    .padding(8)
                    .width(Length::Fill)
                    .on_press(Message::Select(index)),
            );
        }
        let mut content = column![sources].width(Length::Fill).spacing(10);
        content = content.push(
            self.text(if self.prompt.recording {
                "Choose a display, then record it in full or select a region."
            } else if self.prompt.window_capture {
                "Only the selected window will be captured."
            } else if self.selected.iter().any(|index| self.prompt.sources[*index].workspace_id().is_some()) {
                if self.include_shell {
                    "Panels, launcher and notifications are included while the workspace is visible. Switching away or opening overview hides the shell from the stream; the workspace keeps sharing."
                } else {
                    "The workspace keeps sharing when you switch away. Panels, launcher and notifications are excluded."
                }
            } else {
                "Displays share everything visible. Windows share only their own content."
            })
            .size(12)
            .width(Length::Fill),
        );
        if self.can_include_shell() {
            content = content.push(
                row![
                    self.text("Include shell").width(Length::Fill),
                    ferese_theme::controls::switch(self.include_shell, self.palette)
                        .on_press(Message::IncludeShell),
                ]
                .align_y(Alignment::Center),
            );
        }
        if self.prompt.recording {
            content = content.push(
                row![
                    self.text("Select a region").width(Length::Fill),
                    ferese_theme::controls::switch(self.region, self.palette)
                        .on_press(Message::Region),
                ]
                .align_y(Alignment::Center),
            );
            let mut audio = column([]).spacing(4);
            for choices in [
                [Audio::Off, Audio::System],
                [Audio::Microphone, Audio::Both],
            ] {
                let mut choices_row = row([]).spacing(4);
                for choice in choices {
                    choices_row = choices_row.push(
                        ferese_theme::controls::text_button(
                            choice.label(),
                            self.font,
                            self.palette,
                            self.audio == choice,
                        )
                        .width(Length::Fill)
                        .on_press(Message::Audio(choice)),
                    );
                }
                audio = audio.push(choices_row);
            }
            content = content.push(self.text("Audio").size(12)).push(audio).push(
                self.text("Uses the default output and microphone selected in audio settings.")
                    .size(12),
            );
        }
        if self.prompt.persist_mode > 0 && !self.prompt.window_capture {
            let label = if !self.can_remember() {
                "This selection needs approval each time"
            } else if self.prompt.persist_mode == 1 {
                "Allow without asking while this app is running"
            } else {
                "Allow this selection without asking again"
            };

            content = content.push(
                row![
                    self.text(label).size(12).width(Length::Fill),
                    ferese_theme::controls::switch(self.remember, self.palette)
                        .on_press_maybe(self.can_remember().then_some(Message::Remember)),
                ]
                .align_y(Alignment::Center),
            );
        }

        let share = ferese_theme::controls::text_button(
            if self.prompt.recording {
                if self.region {
                    "Select region"
                } else {
                    "Record"
                }
            } else if self.prompt.window_capture {
                "Capture"
            } else {
                "Share"
            },
            self.font,
            self.palette,
            true,
        )
        .on_press_maybe((!self.selected.is_empty()).then_some(Message::Share));
        let cancel = ferese_theme::controls::text_button("Cancel", self.font, self.palette, false)
            .class(ferese_theme::controls::material_button_style(
                self.palette,
                false,
                self.background.a,
            ))
            .on_press(Message::Cancel);
        let footer = row![
            cosmic::iced::widget::Space::new().width(Length::Fill),
            cancel,
            share
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        // Reserve space for the actions even when the compositor shrinks the
        // dialog. The source list and options share the remaining scroll area.
        container(
            column![
                header,
                scrollable(content).height(Length::Fill).width(Length::Fill),
                footer
            ]
            .spacing(10)
            .height(Length::Fill),
        )
        .padding(14)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }
}

impl Picker {
    fn can_include_shell(&self) -> bool {
        self.prompt.allows_shell(
            self.selected
                .iter()
                .map(|index| self.prompt.sources[*index].name.as_str()),
        )
    }

    fn can_remember(&self) -> bool {
        !self.selected.is_empty()
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
mod tests {
    use super::*;

    fn prompt() -> Prompt {
        serde_json::from_value(serde_json::json!({
            "app": "test", "multiple": true, "workspace_shell": true,
            "sources": [
                {"name":"workspace:1", "label":"Workspace 1", "width":800, "height":600, "x":0, "y":0, "scale":1},
                {"name":"DP-1", "label":"Display", "width":800, "height":600, "x":0, "y":0, "scale":1},
                {"name":"window:2", "label":"Window", "width":80, "height":60, "x":0, "y":0, "scale":1}
            ]
        })).unwrap()
    }

    #[test]
    fn shell_requires_a_selected_workspace_and_compositor_support() {
        let mut prompt = prompt();
        assert!(!prompt.allows_shell([]));
        assert!(!prompt.allows_shell(["DP-1", "window:2"]));
        assert!(!prompt.allows_shell(["workspace:99"]));
        assert!(prompt.allows_shell(["workspace:1"]));
        assert!(prompt.allows_shell(["workspace:1", "DP-1"]));
        prompt.workspace_shell = false;
        assert!(!prompt.allows_shell(["workspace:1"]));
        prompt.workspace_shell = true;
        prompt.recording = true;
        assert!(!prompt.allows_shell(["workspace:1"]));
        prompt.recording = false;
        prompt.window_capture = true;
        assert!(!prompt.allows_shell(["workspace:1"]));
    }

    #[test]
    fn older_picker_messages_do_not_enable_shell_capture() {
        let selection: Selection =
            serde_json::from_str(r#"{"names":["workspace:1"],"persist_mode":0}"#).unwrap();
        assert!(!selection.include_shell);
        let mut value = serde_json::to_value(prompt()).unwrap();
        value.as_object_mut().unwrap().remove("workspace_shell");
        let prompt: Prompt = serde_json::from_value(value).unwrap();
        assert!(!prompt.allows_shell(["workspace:1"]));
    }
}
