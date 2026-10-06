use std::io::{Read, Write};

use cosmic::app::{Core, Settings, Task};
use cosmic::iced::{Alignment, Length};
use cosmic::widget::{button, column, container, row, scrollable};
use cosmic::{ApplicationExt, Element};
use ferese_theme::icons::symbolic as glyph;
use serde::{Deserialize, Serialize};

use crate::capture::Source;

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
}

impl Prompt {
    fn source_list_height(&self) -> f32 {
        let visible = &self.sources[..self.sources.len().min(4)];
        let mut sections = 0;
        let mut previous_kind = None;
        if !self.window_capture {
            for source in visible {
                let is_window = source.window_id().is_some();
                if previous_kind != Some(is_window) {
                    sections += 1;
                    previous_kind = Some(is_window);
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
}

#[derive(Clone, Debug)]
enum Message {
    ThemeChanged(Box<ferese_ipc::theme::Snapshot>),
    WindowOpened(cosmic::iced::window::Id),
    Attached(crate::parent::Attachment),
    Select(usize),
    Remember,
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
    background: cosmic::iced::Color,
    palette: ferese_theme::Palette,
    font: cosmic::font::Font,
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    std::io::stdin().take(256 * 1024 + 1).read_to_string(&mut input)?;
    if input.len() > 256 * 1024 {
        return Err("Picker request is too large".into());
    }
    let prompt: Prompt = serde_json::from_str(&input)?;

    if prompt.sources.is_empty() || prompt.sources.len() > 144 {
        return Err("No shareable displays".into());
    }

    let (theme, font, background, palette) = appearance();
    let height = 180. + prompt.source_list_height() + if prompt.persist_mode > 0 { 48. } else { 0. };
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
    type Flags = (Prompt, cosmic::iced::Color, ferese_theme::Palette, cosmic::font::Font);
    type Message = Message;
    const APP_ID: &'static str = "dev.ferese.ScreenShare";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(mut core: Core, (prompt, background, palette, font): Self::Flags) -> (Self, Task<Message>) {
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
            background,
            palette,
            font,
        };
        let task = app
            .core
            .main_window_id()
            .map(|id| {
                app.set_window_title(
                    if app.prompt.window_capture {
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
            ferese_theme_client::service::subscription().map(|snapshot| Message::ThemeChanged(Box::new(snapshot))),
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
                self.font = ferese_theme::font(Some(&snapshot.presented.tokens.typography.font_family));
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
                        // The chooser is still shown, without a parent window.
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
            }
            Message::Remember if self.can_remember() => self.remember = !self.remember,
            Message::Share if !self.selected.is_empty() => {
                let names: Vec<_> = self.selected.iter().map(|i| &self.prompt.sources[*i].name).collect();
                if self.prompt.window_capture {
                    println!("{}", serde_json::to_string(&names).unwrap());
                } else {
                    let selection = Selection {
                        names: names.into_iter().cloned().collect(),
                        persist_mode: if self.remember && self.can_remember() {
                            self.prompt.persist_mode
                        } else {
                            0
                        },
                    };
                    println!("{}", serde_json::to_string(&selection).unwrap());
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
        let mut content = column([]).width(Length::Fill).height(Length::Fill).spacing(10).push(
            row![
                glyph(ferese_theme::icons::DISPLAY, 24),
                column![
                    self.text(if self.prompt.window_capture {
                        "Choose a window"
                    } else {
                        "Choose what to share"
                    })
                    .size(17),
                    self.text(format!(
                        "{} {app}",
                        if self.prompt.window_capture {
                            "Screenshot for"
                        } else {
                            "Share with"
                        }
                    ))
                    .size(12),
                ]
                .spacing(2)
                .width(Length::Fill),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        );
        let mut sources = column([]).spacing(6);
        let mut previous_kind = None;
        for (index, source) in self.prompt.sources.iter().enumerate() {
            let is_window = self.prompt.window_capture || source.window_id().is_some();
            if !self.prompt.window_capture && previous_kind != Some(is_window) {
                sources = sources.push(self.text(if is_window { "Windows" } else { "Displays" }).size(12));
                previous_kind = Some(is_window);
            }
            let selected = self.selected.contains(&index);
            let mut entry = row![
                glyph(ferese_theme::icons::DISPLAY, 24),
                column![
                    self.text(if self.prompt.window_capture || source.window_id().is_some() {
                        &source.label
                    } else {
                        &source.name
                    })
                    .size(14),
                    self.text(format!("{} × {}", source.width, source.height)).size(12)
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
        content = content.push(scrollable(sources).height(Length::Fixed(self.prompt.source_list_height())));
        content = content.push(
            self.text(if self.prompt.window_capture {
                "Only the selected window will be captured."
            } else {
                "Displays share everything visible. Windows share only their own content."
            })
            .size(12)
            .width(Length::Fill),
        );
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
            if self.prompt.window_capture { "Capture" } else { "Share" },
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
        content = content
            .push(cosmic::iced::widget::Space::new().height(Length::Fill))
            .push(
                row![cosmic::iced::widget::Space::new().width(Length::Fill), cancel, share]
                    .spacing(10)
                    .align_y(Alignment::Center),
            );
        container(content).padding(14).width(Length::Fill).into()
    }
}

impl Picker {
    fn can_remember(&self) -> bool {
        !self.selected.is_empty()
            && self.selected.len() <= 16
            && self
                .selected
                .iter()
                .all(|index| self.prompt.rememberable.contains(&self.prompt.sources[*index].name))
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
