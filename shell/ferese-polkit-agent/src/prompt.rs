use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cosmic::app::{Core, Settings, Task};
use cosmic::iced::{Alignment, Color, Length, Size, Subscription, window};
use cosmic::widget::{button, column, container, row, scrollable};
use cosmic::{Element, theme, widget};
use zeroize::{Zeroize, Zeroizing};

use crate::{AuthenticationRequest, Generation, PromptEvent};

#[derive(Clone)]
struct Appearance {
    appearance: ferese_config::theme::Appearance,
    high_contrast: bool,
    surface: Color,
    text: Color,
    muted: Color,
    accent: Color,
    on_accent: Color,
    radius: f32,
    font: cosmic::font::Font,
}

impl Appearance {
    fn load() -> Self {
        let snapshot = ferese_theme_client::service::current();
        Self::from_resolved(&snapshot.presented)
    }

    fn from_resolved(theme: &ferese_config::theme::ResolvedTheme) -> Self {
        let palette = ferese_theme::Palette::from_resolved(theme).flat();
        let family = &theme.tokens.typography.font_family;
        Self {
            appearance: theme.appearance,
            high_contrast: theme.accessibility.increase_contrast,
            surface: Color {
                a: ferese_theme::material_opacity(theme),
                ..palette.sidebar
            },
            text: palette.text,
            muted: palette.muted,
            accent: palette.accent,
            on_accent: palette.on_accent,
            radius: palette.radius,
            font: ferese_theme::font(Some(family)),
        }
    }

    fn palette(&self) -> ferese_theme::Palette {
        ferese_theme::Palette {
            appearance: self.appearance,
            high_contrast: self.high_contrast,
            background: self.surface,
            sidebar: self.surface,
            card: self.surface,
            text: self.text,
            muted: self.muted,
            accent: self.accent,
            accent_gradient: None,
            on_accent: self.on_accent,
            radius: self.radius,
            error: Color::from_rgb8(235, 98, 98),
        }
    }

    fn theme(&self) -> cosmic::Theme {
        self.palette().native_theme()
    }
}

#[derive(Clone)]
enum Message {
    ThemeChanged(Box<ferese_ipc::theme::Snapshot>),
    WindowOpened(cosmic::iced::window::Id),
    MaterialAttached(Result<ferese_theme_client::material::ModalMaterial, String>),
    Tick,
    Input(String),
    Submit,
    SelectIdentity(usize),
    ToggleDetails,
    Cancel,
}

impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthenticationPromptMessage")
    }
}

struct Prompt {
    core: Core,
    material: Option<ferese_theme_client::material::ModalMaterial>,
    appearance: Appearance,
    incoming: Arc<Mutex<VecDeque<PromptEvent>>>,
    request: AuthenticationRequest,
    identity_names: Vec<String>,
    details_expanded: bool,
    action_icon: Option<widget::icon::Handle>,
    question: String,
    echo: bool,
    answer: Zeroizing<String>,
    waiting: bool,
    awaiting_ack: bool,
    error: Option<String>,
    info: Option<String>,
    height: f32,
}

fn content_height(description: &str, status: Option<&str>) -> f32 {
    let lines: usize = description
        .lines()
        .map(|line| line.chars().count().div_ceil(48).max(1))
        .sum();
    let status_height = status.map_or(0, |message| 14 + 18 * message.chars().count().div_ceil(48).clamp(1, 3));
    (214 + 18 * lines.saturating_sub(1) + status_height).min(360) as f32
}

fn dialog_height(request: &AuthenticationRequest, status: Option<&str>, expanded: bool) -> f32 {
    let extra = if request.identities.len() > 1 { 54. } else { 0. }
        + if has_details(request) { 40. } else { 0. }
        + if expanded { 96. } else { 0. };
    (content_height(&request.message, status) + extra).min(360.)
}

fn has_details(request: &AuthenticationRequest) -> bool {
    !request.details.is_empty() || !request.vendor_name.is_empty() || !request.vendor_url.is_empty()
}

fn action_icon(name: &str) -> Option<widget::icon::Handle> {
    if name.is_empty() {
        return None;
    }
    // Resolve once; a missing icon uses Ferese's embedded lock, never a blank header.
    widget::icon::from_name(name.to_owned())
        .size(20)
        .fallback(None)
        .path()
        .map(widget::icon::from_path)
}

fn read_events(lines: impl Iterator<Item = std::io::Result<String>>, queue: &Mutex<VecDeque<PromptEvent>>) {
    for line in lines {
        let Ok(line) = line else { break };
        if let Ok(event) = serde_json::from_str(&line) {
            queue.lock().unwrap().push_back(event);
        }
    }
    // The agent owns this pipe. EOF (including agent death) closes the prompt.
    queue.lock().unwrap().push_back(PromptEvent::Cancel);
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let stdin = std::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    let Some(Ok(first)) = lines.next() else {
        return Err("Missing authentication request".into());
    };
    let PromptEvent::Start(request) = serde_json::from_str(&first)? else {
        return Err("Invalid authentication request".into());
    };
    let incoming = Arc::new(Mutex::new(VecDeque::new()));
    let queue = incoming.clone();
    std::thread::spawn(move || {
        read_events(lines, &queue);
    });
    let appearance = Appearance::load();
    let height = dialog_height(&request, None, false);
    cosmic::app::run::<Prompt>(
        Settings::default()
            .size(Size::new(434., height))
            .client_decorations(false)
            .transparent(true)
            .theme(appearance.theme())
            .default_font(appearance.font)
            .default_text_size(14.)
            .antialiasing(true),
        (incoming, request, appearance),
    )?;
    Ok(())
}

fn reply(mut event: PromptEvent) {
    if let Ok(mut line) = serde_json::to_string(&event) {
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{line}");
        let _ = stdout.flush();
        line.zeroize();
    }
    if let PromptEvent::Response { value, .. } = &mut event {
        value.zeroize();
    }
}

fn focus() -> Task<Message> {
    widget::text_input::focus(widget::Id::new("auth-response"))
}

impl Prompt {
    fn reset_authentication(&mut self, uid: u32) {
        self.request.selected_uid = uid;
        self.answer.zeroize();
        self.error = None;
        self.info = None;
        self.waiting = true;
        self.echo = false;
        self.question = "Waiting for authentication…".into();
    }

    fn select_identity(&mut self, index: usize) -> Option<u32> {
        let uid = self.request.identities.get(index)?.uid;
        if uid == self.request.selected_uid {
            return None;
        }
        self.request.generation = Generation {
            selection: self.request.generation.selection.checked_add(1)?,
            attempt: 0,
        };
        self.reset_authentication(uid);
        self.awaiting_ack = true;
        Some(uid)
    }

    fn resize_to_content(&mut self) -> Task<Message> {
        let height = dialog_height(
            &self.request,
            self.error.as_deref().or(self.info.as_deref()),
            self.details_expanded,
        );
        if (height - self.height).abs() < 1.0 {
            return Task::none();
        }
        self.height = height;
        self.core
            .main_window_id()
            .map(|id| window::resize(id, Size::new(434., height)))
            .unwrap_or_else(Task::none)
    }
}

impl cosmic::Application for Prompt {
    type Executor = cosmic::executor::Default;
    type Flags = (Arc<Mutex<VecDeque<PromptEvent>>>, AuthenticationRequest, Appearance);
    type Message = Message;
    const APP_ID: &'static str = "dev.ferese.Authentication";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(self.appearance.palette().application_style(if self.material.is_some() {
            cosmic::iced::Color::TRANSPARENT
        } else {
            self.appearance.surface
        }))
    }

    fn init(mut core: Core, (incoming, request, appearance): Self::Flags) -> (Self, Task<Message>) {
        core.window.show_headerbar = false;
        core.window.border_padding = Some(0);
        core.window.content_container = false;
        core.window.use_template = false;
        let height = dialog_height(&request, None, false);
        let identity_names = request
            .identities
            .iter()
            .map(|identity| identity.name.clone())
            .collect();
        let action_icon = action_icon(&request.icon_name);
        let bounds = core
            .main_window_id()
            .map(|id| window::set_max_size(id, Some(Size::new(434., 360.))))
            .unwrap_or_else(Task::none);
        (
            Self {
                core,
                material: None,
                appearance,
                incoming,
                request,
                identity_names,
                details_expanded: false,
                action_icon,
                question: "Waiting for authentication…".into(),
                echo: false,
                answer: Zeroizing::new(String::new()),
                waiting: true,
                awaiting_ack: true,
                error: None,
                info: None,
                height,
            },
            Task::batch([focus(), bounds]),
        )
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            ferese_theme_client::service::subscription().map(|snapshot| Message::ThemeChanged(Box::new(snapshot))),
            cosmic::iced::time::every(Duration::from_millis(30)).map(|_| Message::Tick),
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
                self.appearance = Appearance::from_resolved(&snapshot.presented);
                return cosmic::command::set_theme(self.appearance.theme());
            }
            Message::WindowOpened(id) => {
                return cosmic::iced::window::run(id, ferese_theme_client::material::ModalMaterial::attach)
                    .map(|result| cosmic::Action::App(Message::MaterialAttached(result)));
            }
            Message::MaterialAttached(result) => {
                self.material = result.ok();
            }
            Message::Tick => {
                let events: Vec<_> = self.incoming.lock().unwrap().drain(..).collect();
                let mut refocus = false;
                for event in events {
                    match event {
                        PromptEvent::IdentityChanged { generation, uid }
                            if uid == self.request.selected_uid
                                && generation.selection == self.request.generation.selection
                                && (generation.attempt > self.request.generation.attempt
                                    || (generation == self.request.generation && self.awaiting_ack)) =>
                        {
                            self.request.generation = generation;
                            self.reset_authentication(uid);
                            self.awaiting_ack = false;
                        }
                        PromptEvent::Request {
                            generation,
                            prompt,
                            echo,
                        } if generation == self.request.generation && !self.awaiting_ack => {
                            self.answer.zeroize();
                            self.question = prompt;
                            self.echo = echo;
                            self.waiting = false;
                            refocus = true;
                        }
                        PromptEvent::Info { generation, text }
                            if generation == self.request.generation && !self.awaiting_ack =>
                        {
                            self.info = Some(text)
                        }
                        PromptEvent::Error { generation, text }
                            if generation == self.request.generation && !self.awaiting_ack =>
                        {
                            self.error = Some(text)
                        }
                        PromptEvent::Cancel => {
                            self.answer.zeroize();
                            self.waiting = true;
                            return cosmic::iced::exit();
                        }
                        _ => {}
                    }
                }
                let resize = self.resize_to_content();
                if refocus {
                    return Task::batch([resize, focus()]);
                }
                return resize;
            }
            Message::Input(mut value) => {
                if !self.waiting && value.len() <= 4096 {
                    self.answer.zeroize();
                    *self.answer = std::mem::take(&mut value);
                    self.error = None;
                }
                value.zeroize();
            }
            Message::Submit if !self.waiting && !self.answer.is_empty() => {
                let value = std::mem::take(&mut *self.answer);
                reply(PromptEvent::Response {
                    generation: self.request.generation,
                    uid: self.request.selected_uid,
                    value,
                });
                self.waiting = true;
            }
            Message::SelectIdentity(index) => {
                if let Some(uid) = self.select_identity(index) {
                    reply(PromptEvent::SelectIdentity {
                        generation: self.request.generation,
                        uid,
                    });
                }
            }
            Message::ToggleDetails => {
                self.details_expanded = !self.details_expanded;
                return self.resize_to_content();
            }
            Message::Cancel => {
                self.answer.zeroize();
                reply(PromptEvent::Cancel);
                return cosmic::iced::exit();
            }
            _ => {}
        }
        Task::none()
    }

    fn on_escape(&mut self) -> Task<Message> {
        self.update(Message::Cancel)
    }

    fn view(&self) -> Element<'_, Message> {
        let palette = &self.appearance;
        let mut input = widget::text_input(&self.question, self.answer.as_str())
            .id(widget::Id::new("auth-response"))
            .font(palette.font)
            .padding([7, 12])
            .style(ferese_theme::controls::authentication_input(palette.palette()));
        if !self.echo {
            input = input.password();
        }
        if !self.waiting {
            input = input.on_input(Message::Input).on_submit(|mut value| {
                value.zeroize();
                Message::Submit
            });
        }
        let action = button::custom(ferese_theme::text("Authenticate", palette.font).size(13))
            .class(ferese_theme::controls::authentication_button(palette.palette()))
            .height(Length::Fixed(36.))
            .padding([8, 16])
            .on_press_maybe((!self.waiting && !self.answer.is_empty()).then_some(Message::Submit));
        let lock_icon = if let Some(icon) = &self.action_icon {
            widget::icon(icon.clone()).size(20)
        } else {
            ferese_theme::icons::tinted(ferese_theme::icons::LOCK, 20, palette.accent)
        };
        let selected = self
            .request
            .identities
            .iter()
            .position(|identity| identity.uid == self.request.selected_uid);
        let user = selected
            .map(|index| self.request.identities[index].name.as_str())
            .unwrap_or("Unknown account");
        let heading = row![
            container(lock_icon)
                .width(40)
                .height(40)
                .center_x(40)
                .center_y(40)
                .class(ferese_theme::controls::surface(palette.accent.scale_alpha(0.13), 12.,)),
            column![
                ferese_theme::text("Authentication required", palette.font).size(18),
                ferese_theme::text(format!("Confirm as {user}"), palette.font)
                    .size(12)
                    .class(theme::Text::Color(palette.muted)),
            ]
            .spacing(2),
        ]
        .spacing(12)
        .align_y(Alignment::Center);
        let mut information = column![ferese_theme::text(&self.request.message, palette.font).size(13)]
            .spacing(10)
            .width(Length::Fill);
        if has_details(&self.request) {
            information = information.push(
                button::custom(
                    ferese_theme::text(
                        if self.details_expanded {
                            "Hide details"
                        } else {
                            "Show details"
                        },
                        palette.font,
                    )
                    .size(12),
                )
                .class(ferese_theme::controls::button_style(palette.palette(), false))
                .padding([6, 10])
                .on_press(Message::ToggleDetails),
            );
            if self.details_expanded {
                let mut details = column![].spacing(6).width(Length::Fill);
                for (key, value) in &self.request.details {
                    details = details.push(detail_row(key, value, palette));
                }
                for (label, value) in [
                    ("Vendor", &self.request.vendor_name),
                    ("Vendor URL", &self.request.vendor_url),
                ] {
                    if !value.is_empty() {
                        details = details.push(detail_row(label, value, palette));
                    }
                }
                information = information.push(details);
            }
        }
        let mut body = column![heading, scrollable(information).height(Length::Fill)]
            .spacing(14)
            .width(Length::Fill);
        if self.request.identities.len() > 1 {
            body = body.push(
                container(
                    row![
                        ferese_theme::text("Account", palette.font)
                            .size(12)
                            .class(theme::Text::Color(palette.muted)),
                        widget::dropdown(&self.identity_names[..], selected, Message::SelectIdentity)
                            .width(Length::Fill),
                        ferese_theme::icons::tinted(ferese_theme::icons::CHEVRON_DOWN, 16, palette.muted),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                )
                .padding([4, 10])
                .class(ferese_theme::controls::surface(palette.accent.scale_alpha(0.08), 8.)),
            );
        }
        body = body.push(input);
        if let Some((status, color)) = self
            .error
            .as_deref()
            .map(|text| (text, Color::from_rgb8(235, 98, 98)))
            .or_else(|| self.info.as_deref().map(|text| (text, palette.muted)))
        {
            let lines: usize = status
                .lines()
                .map(|line| line.chars().count().div_ceil(48).max(1))
                .sum();
            body = body.push(
                scrollable(
                    ferese_theme::text(status, palette.font)
                        .size(12)
                        .class(theme::Text::Color(color)),
                )
                .height(Length::Fixed(18. * lines.clamp(1, 3) as f32)),
            );
        }
        body = body.push(
            row![
                button::custom(ferese_theme::text("Cancel", palette.font).size(13))
                    .class(ferese_theme::controls::button_style(palette.palette(), false))
                    .padding([8, 14])
                    .on_press(Message::Cancel),
                action
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );
        container(body)
            .padding(20)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

fn detail_row<'a>(key: &'a str, value: &'a str, palette: &Appearance) -> Element<'a, Message> {
    row![
        ferese_theme::text(format!("{key}:"), palette.font)
            .size(12)
            .class(theme::Text::Color(palette.muted))
            .width(Length::Fixed(106.)),
        ferese_theme::text(value, palette.font).size(12).width(Length::Fill),
    ]
    .spacing(8)
    .into()
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use cosmic::app::Core;

    use super::{Appearance, Message, Prompt, action_icon, content_height, dialog_height, has_details};
    use crate::tests::request_fixture;

    #[test]
    fn agent_pipe_eof_cancels_the_prompt_and_clears_the_password() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;

        use cosmic::Application;

        use crate::PromptEvent;

        let (reader, mut writer) = UnixStream::pair().unwrap();
        let incoming = Arc::new(Mutex::new(VecDeque::new()));
        let queue = incoming.clone();
        let reader_thread = std::thread::spawn(move || super::read_events(BufReader::new(reader).lines(), &queue));
        writeln!(
            writer,
            "{{\"type\":\"request\",\"generation\":{{\"selection\":0,\"attempt\":0}},\"prompt\":\"Password:\",\"echo\":false}}"
        )
        .unwrap();
        drop(writer);
        reader_thread.join().unwrap();
        assert!(matches!(incoming.lock().unwrap().back(), Some(PromptEvent::Cancel)));
        let appearance = Appearance::from_resolved(&ferese_config::theme::default_theme());
        let (mut prompt, _) = Prompt::init(Core::default(), (incoming, request_fixture(), appearance));
        *prompt.answer = "test-password".into();
        prompt.waiting = false;
        let _ = prompt.update(Message::Tick);
        assert!(prompt.answer.is_empty());
        assert!(prompt.waiting);
    }

    #[test]
    fn stale_prompts_and_acknowledgements_cannot_undo_a_newer_selection() {
        use cosmic::Application;

        use crate::{Generation, PromptEvent};

        let incoming = Arc::new(Mutex::new(VecDeque::new()));
        let appearance = Appearance::from_resolved(&ferese_config::theme::default_theme());
        let (mut prompt, _) = Prompt::init(Core::default(), (incoming.clone(), request_fixture(), appearance));
        assert_eq!(prompt.select_identity(1), Some(1001));
        let older = prompt.request.generation;
        assert_eq!(prompt.select_identity(0), Some(1000));
        let current = prompt.request.generation;
        incoming.lock().unwrap().extend([
            PromptEvent::IdentityChanged {
                generation: older,
                uid: 1001,
            },
            PromptEvent::Request {
                generation: older,
                prompt: "Old visible prompt".into(),
                echo: true,
            },
            PromptEvent::IdentityChanged {
                generation: Generation::default(),
                uid: 1000,
            },
            PromptEvent::Request {
                generation: Generation::default(),
                prompt: "Original visible prompt".into(),
                echo: true,
            },
            PromptEvent::Info {
                generation: older,
                text: "Old info".into(),
            },
            PromptEvent::Error {
                generation: older,
                text: "Old error".into(),
            },
        ]);
        let _ = prompt.update(Message::Tick);
        assert_eq!(prompt.request.selected_uid, 1000);
        assert_eq!(prompt.request.generation, current);
        assert!(prompt.waiting && prompt.awaiting_ack);
        assert!(!prompt.echo);
        assert!(prompt.info.is_none() && prompt.error.is_none());
        incoming.lock().unwrap().extend([
            PromptEvent::IdentityChanged {
                generation: current,
                uid: 1000,
            },
            PromptEvent::Request {
                generation: current,
                prompt: "Current password".into(),
                echo: false,
            },
            PromptEvent::IdentityChanged {
                generation: current,
                uid: 1000,
            },
            PromptEvent::Request {
                generation: older,
                prompt: "Delayed visible prompt".into(),
                echo: true,
            },
        ]);
        let _ = prompt.update(Message::Tick);
        assert!(!prompt.waiting && !prompt.awaiting_ack && !prompt.echo);
        assert_eq!(prompt.question, "Current password");

        // A fresh retry gets its own generation, even for the same account.
        let retry = Generation { attempt: 1, ..current };
        incoming.lock().unwrap().extend([
            PromptEvent::IdentityChanged {
                generation: retry,
                uid: 1000,
            },
            PromptEvent::Request {
                generation: retry,
                prompt: "Retry password".into(),
                echo: false,
            },
            PromptEvent::Request {
                generation: current,
                prompt: "Old retry prompt".into(),
                echo: true,
            },
            PromptEvent::IdentityChanged {
                generation: current,
                uid: 1000,
            },
        ]);
        let _ = prompt.update(Message::Tick);
        assert_eq!(prompt.request.generation, retry);
        assert_eq!(prompt.question, "Retry password");
        assert!(!prompt.waiting && !prompt.echo);
    }

    #[test]
    fn selecting_another_account_clears_password_and_waits_for_a_fresh_prompt() {
        let appearance = Appearance::from_resolved(&ferese_config::theme::default_theme());
        let (mut prompt, _) = <Prompt as cosmic::Application>::init(
            Core::default(),
            (Arc::new(Mutex::new(VecDeque::new())), request_fixture(), appearance),
        );
        *prompt.answer = "test-response".into();
        prompt.waiting = false;
        prompt.echo = true;
        prompt.error = Some("Old account error".into());
        assert_eq!(prompt.select_identity(1), Some(1001));
        assert!(prompt.answer.is_empty());
        assert!(prompt.error.is_none());
        assert!(prompt.waiting);
        assert!(!prompt.echo);
        assert_eq!(prompt.request.selected_uid, 1001);
        assert_eq!(prompt.select_identity(1), None);
        assert_eq!(prompt.select_identity(20), None);
        let _ = <Prompt as cosmic::Application>::update(&mut prompt, Message::ToggleDetails);
        assert!(prompt.details_expanded);
        assert!(prompt.height <= 360.);
    }

    #[test]
    fn full_requests_and_expanded_details_keep_the_height_cap() {
        let request = request_fixture();
        assert_eq!(dialog_height(&request, None, false), 360.);
        assert_eq!(dialog_height(&request, Some("Password not accepted"), true), 360.);
        assert!(has_details(&request));
        let mut short = request;
        short.message = "Short request".into();
        short.identities.truncate(1);
        assert!(dialog_height(&short, None, true) > dialog_height(&short, None, false));
        short.details.clear();
        short.vendor_name.clear();
        short.vendor_url.clear();
        assert!(!has_details(&short));
    }

    #[test]
    fn absent_or_unknown_action_icons_use_the_embedded_lock() {
        assert!(action_icon("").is_none());
        assert!(action_icon("ferese-nonexistent-action-icon-for-test").is_none());
    }

    #[test]
    fn dialog_grows_for_long_messages_and_caps_its_height() {
        assert_eq!(content_height("Short request", None), 214.);
        assert!(content_height(&"Long message ".repeat(20), None) > 214.);
        assert_eq!(content_height(&"Long message ".repeat(100), None), 360.);
        assert!(content_height("Short request", Some("Password not accepted")) > 214.);
    }
}
