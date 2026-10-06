use std::io::{Read, Write};
use std::sync::Arc;

use cosmic::Element;
use cosmic::app::{Core, Settings, Task};
use cosmic::iced::{Alignment, Length};
use cosmic::widget::{button, column, container, image, row, scrollable, text_input};
use ferese_theme::{Palette, accent_button, controls, text};
use ferese_theme_client::material::ModalMaterial;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Prompt {
    pub title: String,
    pub description: String,
    pub accept: String,
    pub parent: String,
    pub image: Option<std::path::PathBuf>,
    #[serde(default)]
    pub shortcuts: Vec<ShortcutField>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ShortcutField {
    pub id: String,
    pub description: String,
    pub trigger: String,
}

#[derive(Clone, Debug)]
enum Message {
    ThemeChanged(Box<ferese_ipc::theme::Snapshot>),
    Opened(cosmic::iced::window::Id),
    Attached(crate::parent::Attachment),
    Accept,
    Cancel,
    Shortcut(usize, String),
}

struct Consent {
    core: Core,
    prompt: Prompt,
    material: Option<ModalMaterial>,
    parent: Option<Arc<crate::parent::Parent>>,
    palette: Palette,
    font: cosmic::font::Font,
    background: cosmic::iced::Color,
}

pub(crate) fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    std::io::stdin().take(256 * 1024 + 1).read_to_string(&mut input)?;

    if input.len() > 256 * 1024 {
        return Err("Consent request is too large".into());
    }

    let prompt: Prompt = serde_json::from_str(&input)?;
    let (theme, font, background, _) = crate::picker::appearance();
    let height = if prompt.image.is_some() || !prompt.shortcuts.is_empty() {
        430.
    } else {
        220.
    };
    cosmic::app::run::<Consent>(
        Settings::default()
            .size(cosmic::iced::Size::new(440., height))
            .client_decorations(false)
            .transparent(true)
            .theme(theme)
            .default_font(font)
            .default_text_size(14.)
            .is_daemon(false),
        (prompt, font, background),
    )?;

    Ok(())
}

impl cosmic::Application for Consent {
    type Executor = cosmic::executor::Default;
    type Flags = (Prompt, cosmic::font::Font, cosmic::iced::Color);
    type Message = Message;
    const APP_ID: &'static str = "dev.ferese.PortalDialog";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(mut core: Core, (prompt, font, background): Self::Flags) -> (Self, Task<Message>) {
        core.window.show_headerbar = false;
        core.window.border_padding = Some(0);
        core.window.content_container = false;
        core.window.use_template = false;
        let app = Self {
            core,
            prompt,
            material: None,
            parent: None,
            palette: Palette::from_resolved(&ferese_theme_client::service::current().presented),
            font,
            background,
        };
        (app, Task::none())
    }

    fn subscription(&self) -> cosmic::iced::Subscription<Message> {
        cosmic::iced::Subscription::batch([
            ferese_theme_client::service::subscription().map(|snapshot| Message::ThemeChanged(Box::new(snapshot))),
            cosmic::iced::event::listen_with(|event, _, id| match event {
                cosmic::iced::Event::Window(cosmic::iced::window::Event::Opened { .. }) => Some(Message::Opened(id)),
                cosmic::iced::Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed {
                    key: cosmic::iced::keyboard::Key::Named(cosmic::iced::keyboard::key::Named::Escape),
                    ..
                }) => Some(Message::Cancel),
                _ => None,
            }),
        ])
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::ThemeChanged(snapshot) => {
                self.palette = Palette::from_resolved(&snapshot.presented).flat();
                self.font = ferese_theme::font(Some(&snapshot.presented.tokens.typography.font_family));
                self.background = cosmic::iced::Color {
                    a: ferese_theme::material_opacity(&snapshot.presented),
                    ..self.palette.sidebar
                };
                return cosmic::command::set_theme(self.palette.native_theme());
            }
            Message::Opened(id) => {
                let parent = self.prompt.parent.clone();
                return cosmic::iced::window::run(id, move |window| {
                    let attached = crate::parent::Parent::attach(window, &parent)?;
                    if let Some(diagnostic) = attached.diagnostic {
                        eprintln!("ferese portal: {diagnostic}");
                    }
                    Ok((attached.parent.map(Arc::new), ModalMaterial::attach(window)))
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
            Message::Accept => {
                if self.prompt.shortcuts.is_empty() {
                    println!("true");
                } else {
                    println!("{}", serde_json::to_string(&self.prompt.shortcuts).unwrap());
                }
                let _ = std::io::stdout().flush();
                return cosmic::iced::exit();
            }
            Message::Cancel => return cosmic::iced::exit(),
            Message::Shortcut(index, value) => {
                if let Some(shortcut) = self.prompt.shortcuts.get_mut(index) {
                    shortcut.trigger = value;
                }
            }
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
        let text = |value: String| text(value, self.font);
        let mut details = column![text(self.prompt.description.clone())].spacing(12);
        for (index, shortcut) in self.prompt.shortcuts.iter().enumerate() {
            details = details.push(
                column![
                    text(shortcut.description.clone()),
                    text_input("Ctrl+Alt+k (leave empty to disable)", &shortcut.trigger)
                        .font(self.font)
                        .style(controls::settings_input(self.palette))
                        .on_input(move |value| Message::Shortcut(index, value))
                ]
                .spacing(6),
            );
        }
        let mut content = column![
            text(self.prompt.title.clone()).size(22),
            scrollable(details).height(Length::Fill)
        ]
        .spacing(16);
        if let Some(path) = &self.prompt.image {
            content = content.push(
                image(image::Handle::from_path(path))
                    .height(Length::Fill)
                    .width(Length::Fill)
                    .content_fit(cosmic::iced::ContentFit::Contain),
            );
        }
        content = content.push(
            row![
                button::custom(text("Cancel".into()))
                    .class(cosmic::theme::Button::Text)
                    .on_press(Message::Cancel),
                button::custom(text(self.prompt.accept.clone()))
                    .class(accent_button())
                    .on_press(Message::Accept)
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        );
        container(content).padding(24).width(Length::Fill).into()
    }
}
