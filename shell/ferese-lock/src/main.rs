mod appearance;
mod auth;
#[cfg(test)]
mod lock_tests;
mod password;
#[cfg(test)]
mod refusal_tests;
mod runtime;

use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use cosmic::app::{Core, Settings, Task};
use cosmic::iced::event::{PlatformSpecific, wayland};
use cosmic::iced::platform_specific::shell::commands::session_lock;
use cosmic::iced::{Event, Length, Subscription, event, window};
use cosmic::widget::{column, container, image, row};
use cosmic::{Element, iced, theme, widget};
use wayland_client::protocol::wl_output::WlOutput;
use zeroize::Zeroizing;

use appearance::Appearance;
use password::Password;

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--help"] {
        println!(
            "Usage: ferese-lock [--preview | --foreground]\nWithout arguments, lock the session using PAM.\n--preview opens an ordinary window; it never locks or authenticates."
        );
        return Ok(());
    }
    let preview = args == ["--preview"]
        || (args.len() == 3 && args[0] == "--preview" && args[1] == "--config");
    let config = if preview && args.len() == 3 {
        Some(PathBuf::from(&args[2]))
    } else {
        None
    };

    if !args.is_empty() && !preview && args != ["--foreground"] {
        eprintln!("Usage: ferese-lock [--preview | --foreground]");
        std::process::exit(2);
    }

    if !preview && !auth::available() {
        eprintln!("ferese-lock: install the ferese-lock PAM policy before locking");
        std::process::exit(1);
    }

    if !preview {
        if let Err(error) = runtime::check_protocol() {
            eprintln!("ferese-lock: {error}");
            std::process::exit(1);
        }

        if args.is_empty() {
            if let Err(error) = runtime::daemonize() {
                eprintln!("ferese-lock: {error}");
                std::process::exit(1);
            }
            return Ok(());
        }
    }

    let user = auth::username().unwrap_or_else(|error| {
        eprintln!("ferese-lock: {error}");
        std::process::exit(1)
    });

    // Password-bearing UI processes must not create core dumps.
    // SAFETY: PR_SET_DUMPABLE takes an integer value and does not dereference caller memory.
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }

    let appearance = Appearance::fallback();
    let completed = Arc::new(AtomicBool::new(preview));
    cosmic::app::run::<Locker>(
        Settings::default()
            .no_main_window(!preview)
            .is_daemon(!preview)
            .client_decorations(false)
            .default_text_size(14.)
            .default_font(appearance.font)
            .theme(appearance.theme())
            .antialiasing(true),
        Flags {
            password: Password::try_new()?,
            preview,
            user,
            config,
            completed: completed.clone(),
        },
    )?;
    runtime::check_completion(completed.load(Ordering::Acquire))?;
    Ok(())
}

struct Flags {
    password: Password,
    preview: bool,
    user: String,
    config: Option<PathBuf>,
    completed: Arc<AtomicBool>,
}

#[derive(Clone)]
enum Message {
    AppearanceLoaded(Result<Box<Appearance>, String>),
    ThemeChanged(Box<ferese_ipc::theme::Snapshot>),
    WallpaperLoaded(u64, Result<widget::image::Handle, String>),
    Event(Box<Event>),
    Input(Zeroizing<String>),
    Backspace,
    ClearPassword,
    InputExpired(Instant),
    Submit,
    Authenticated(bool),
    Tick,
    RetryReady,
    FocusPassword,
}

// Never derive Debug for password-bearing messages.
impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LockerMessage")
    }
}

#[derive(Default, Debug, PartialEq)]
enum AuthState {
    #[default]
    Ready,
    Checking,
    Rejected(Instant),
    Unlocking,
}

impl AuthState {
    fn may_submit(&self) -> bool {
        matches!(self, Self::Ready)
    }

    fn retry_ready(&mut self, now: Instant) -> bool {
        if matches!(self, Self::Rejected(at) if now.saturating_duration_since(*at) >= Duration::from_secs(2))
        {
            *self = Self::Ready;
            true
        } else {
            false
        }
    }

    fn complete(&mut self, success: bool) -> bool {
        if *self != Self::Checking {
            return false;
        }
        *self = if success {
            Self::Unlocking
        } else {
            Self::Rejected(Instant::now())
        };
        success
    }
}

struct Locker {
    core: Core,
    preview: bool,
    user: String,
    appearance: Appearance,
    appearance_loaded: bool,
    config: Option<PathBuf>,
    completed: Arc<AtomicBool>,
    password: Password,
    input_at: Option<Instant>,
    state: AuthState,
    status: &'static str,
    outputs: HashMap<WlOutput, window::Id>,
    started: bool,
    clock: String,
    date: String,
    confirmation: runtime::Confirmation,
    caps_lock: bool,
}

impl cosmic::Application for Locker {
    type Executor = cosmic::executor::Default;
    type Flags = Flags;
    type Message = Message;

    const APP_ID: &'static str = "dev.ferese.Lock";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(mut core: Core, flags: Self::Flags) -> (Self, Task<Message>) {
        let Flags {
            password,
            preview,
            user,
            config,
            completed,
        } = flags;
        let appearance = Appearance::fallback();
        let artwork = if preview {
            load_appearance(user.clone(), config.clone())
        } else {
            Task::none()
        };
        core.window.show_headerbar = false;
        let now = jiff::Zoned::now();
        let clock = now.strftime(appearance.clock_format()).to_string();
        let title_task = core
            .main_window_id()
            .map(|id| cosmic::command::set_title(id, "Ferese Lock Preview".to_owned()))
            .unwrap_or_else(Task::none);
        (
            Self {
                core,
                preview,
                user,
                appearance,
                appearance_loaded: false,
                config,
                completed,
                password,
                input_at: None,
                state: AuthState::Ready,
                status: "Press Enter to unlock",
                outputs: HashMap::new(),
                started: false,
                confirmation: Default::default(),
                caps_lock: false,
                clock,
                date: now.strftime("%A, %B %-d").to_string(),
            },
            if preview {
                Task::batch([title_task, focus(), artwork])
            } else {
                session_lock::lock()
            },
        )
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            if self.preview || !self.appearance_loaded {
                Subscription::none()
            } else {
                ferese_theme_client::service::subscription()
                    .map(|snapshot| Message::ThemeChanged(Box::new(snapshot)))
            },
            event::listen_with(|event, _, _| match event {
                Event::Keyboard(iced::keyboard::Event::KeyPressed {
                    key,
                    modifiers,
                    text,
                    ..
                }) => {
                    use iced::keyboard::{Key, key::Named};
                    match key {
                        Key::Named(Named::Enter) => Some(Message::Submit),
                        Key::Named(Named::Escape) => Some(Message::ClearPassword),
                        Key::Named(Named::Backspace) if modifiers.control() => {
                            Some(Message::ClearPassword)
                        }
                        Key::Named(Named::Backspace) => Some(Message::Backspace),
                        Key::Character(ref key) if modifiers.control() && key.as_str() == "u" => {
                            Some(Message::ClearPassword)
                        }
                        _ if !modifiers.control() && !modifiers.logo() => {
                            text.map(|text| Message::Input(Zeroizing::new(text.to_string())))
                        }
                        _ => None,
                    }
                }
                Event::PlatformSpecific(PlatformSpecific::Wayland(
                    wayland::Event::Output(..) | wayland::Event::SessionLock(..),
                ))
                | Event::Window(window::Event::Opened { .. })
                | Event::Keyboard(iced::keyboard::Event::ModifiersChanged(_)) => {
                    Some(Message::Event(Box::new(event)))
                }
                _ => None,
            }),
            if self.appearance.show_clock || self.appearance.show_date {
                Subscription::run(runtime::minute_ticks).map(|_| Message::Tick)
            } else {
                Subscription::none()
            },
            self.input_at.map_or_else(Subscription::none, |at| {
                Subscription::run_with(at, runtime::password_deadline).map(Message::InputExpired)
            }),
            match self.state {
                AuthState::Rejected(at) => {
                    Subscription::run_with(at, runtime::retry_deadline).map(|_| Message::RetryReady)
                }
                _ => Subscription::none(),
            },
        ])
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::AppearanceLoaded(result) => {
                match result {
                    Ok(appearance) => self.appearance = *appearance,
                    Err(error) => eprintln!("ferese-lock: using fallback appearance: {error}"),
                }
                self.appearance_loaded = true;
                let now = jiff::Zoned::now();
                self.clock = now.strftime(self.appearance.clock_format()).to_string();
                return cosmic::command::set_theme(self.appearance.theme());
            }
            Message::ThemeChanged(snapshot) => {
                let palette = ferese_theme::Palette::from_resolved(&snapshot.presented);
                self.appearance.appearance = snapshot.presented.appearance;
                self.appearance.panel = palette.sidebar;
                self.appearance.text = palette.text;
                self.appearance.accent = palette.accent;
                self.appearance.on_accent = palette.on_accent;
                self.appearance.radius = palette.radius;
                self.appearance.font =
                    ferese_theme::font(Some(&snapshot.presented.tokens.typography.font_family));
                let theme = cosmic::command::set_theme(self.appearance.theme());
                if let Some((revision, path, blur)) =
                    self.appearance.request_wallpaper(&snapshot.theme)
                {
                    let wallpaper = cosmic::task::future(async move {
                        let result = tokio::task::spawn_blocking(move || {
                            appearance::load_wallpaper(&path, blur)
                        })
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()));
                        Message::WallpaperLoaded(revision, result)
                    });
                    return Task::batch([theme, wallpaper]);
                }
                return theme;
            }
            Message::WallpaperLoaded(revision, result) => {
                if let Err(error) = self.appearance.finish_wallpaper(revision, result) {
                    eprintln!("ferese-lock: retained wallpaper: {error}");
                }
            }
            Message::Event(event) => return self.handle_event(*event),
            Message::Tick => {
                let now = jiff::Zoned::now();
                self.clock = now.strftime(self.appearance.clock_format()).to_string();
                self.date = now.strftime("%A, %B %-d").to_string();
            }
            Message::RetryReady => {
                if self.state.retry_ready(Instant::now()) {
                    return focus();
                }
            }
            Message::FocusPassword => {
                if self.state.may_submit() {
                    return focus();
                }
            }
            Message::Input(value) => {
                if self.state.may_submit() && !value.is_empty() && self.password.push_str(&value) {
                    self.input_at = Some(Instant::now());
                    self.status = "Press Enter to unlock";
                }
            }
            Message::Backspace => {
                if self.state.may_submit() {
                    self.password.pop();
                    self.input_at = (!self.password.is_empty()).then(Instant::now);
                }
            }
            Message::ClearPassword => {
                self.password.clear();
                self.input_at = None;
            }
            Message::InputExpired(at) => {
                if self.input_at == Some(at) {
                    self.password.clear();
                    self.input_at = None;
                }
            }
            Message::Submit => {
                if self.preview {
                    self.password.clear();
                    self.input_at = None;
                    self.status = "Preview only · your session is not locked";
                    return focus();
                }

                if !self.confirmation.confirmed()
                    || !self.state.may_submit()
                    || self.password.is_empty()
                {
                    return Task::none();
                }

                let replacement = match Password::try_new() {
                    Ok(password) => password,
                    Err(error) => {
                        eprintln!("ferese-lock: cannot lock password memory: {error}");
                        self.status = "Cannot prepare authentication. Try again.";
                        return Task::none();
                    }
                };
                self.state = AuthState::Checking;
                self.status = "Checking password…";
                self.input_at = None;
                let password = std::mem::replace(&mut self.password, replacement);
                let user = self.user.clone();
                return Task::perform(
                    async move {
                        tokio::task::spawn_blocking(move || auth::authenticate(user, password))
                            .await
                            .unwrap_or(false)
                    },
                    |accepted| cosmic::Action::App(Message::Authenticated(accepted)),
                );
            }
            Message::Authenticated(accepted) => {
                if self.state.complete(accepted) && !self.preview {
                    return session_lock::unlock();
                }
                self.status = "Password not accepted. Try again.";
                return focus();
            }
        }

        Task::none()
    }

    fn on_escape(&mut self) -> Task<Message> {
        self.password.clear();
        self.input_at = None;
        focus()
    }

    fn view(&self) -> Element<'_, Message> {
        self.screen()
    }

    fn view_window(&self, _: window::Id) -> Element<'_, Message> {
        self.screen()
    }
}

fn load_appearance(user: String, config: Option<PathBuf>) -> Task<Message> {
    cosmic::task::future(async move {
        let result = tokio::task::spawn_blocking(move || {
            let config = config.or_else(ferese_config::config_path);
            Box::new(Appearance::load(&user, config.as_deref()))
        })
        .await
        .map_err(|error| error.to_string());
        Message::AppearanceLoaded(result)
    })
}

fn focus() -> Task<Message> {
    widget::text_input::focus(widget::Id::new("password"))
}

impl Locker {
    fn handle_event(&mut self, event: Event) -> Task<Message> {
        match event {
            Event::Keyboard(iced::keyboard::Event::ModifiersChanged(modifiers)) => {
                self.caps_lock = modifiers.contains(iced::keyboard::Modifiers::CAPS_LOCK);
            }
            Event::PlatformSpecific(PlatformSpecific::Wayland(wayland::Event::Output(
                event,
                output,
            ))) if !self.preview => {
                if matches!(event, wayland::OutputEvent::Removed) {
                    if let Some(id) = self.outputs.remove(&output) {
                        return session_lock::destroy_lock_surface(id);
                    }
                } else if !self.outputs.contains_key(&output) {
                    let id = window::Id::unique();
                    self.outputs.insert(output.clone(), id);
                    if self.started {
                        return session_lock::get_lock_surface(id, output);
                    }
                }
            }
            Event::PlatformSpecific(PlatformSpecific::Wayland(wayland::Event::SessionLock(
                event,
            ))) if !self.preview => match event {
                wayland::SessionLockEvent::Locked if self.confirmation.observe(&event) => {
                    use std::io::Write;
                    if std::env::var_os("FERESE_LOCK_READY").is_some() {
                        let _ = writeln!(std::io::stdout().lock(), "FERESE_LOCKED");
                    }
                    return load_appearance(self.user.clone(), self.config.clone());
                }
                wayland::SessionLockEvent::Requested if !self.started => {
                    self.started = true;
                    return Task::batch(
                        self.outputs.iter().map(|(output, id)| {
                            session_lock::get_lock_surface(*id, output.clone())
                        }),
                    );
                }
                wayland::SessionLockEvent::Focused(..) => return focus(),
                wayland::SessionLockEvent::Unlocked if self.state == AuthState::Unlocking => {
                    self.completed.store(true, Ordering::Release);
                    return iced::exit();
                }
                wayland::SessionLockEvent::Finished | wayland::SessionLockEvent::NotSupported => {
                    eprintln!("ferese-lock: compositor refused the lock");
                    return iced::exit();
                }
                _ => {}
            },
            Event::Window(window::Event::Opened { .. }) => {
                return focus();
            }
            _ => {}
        }

        Task::none()
    }

    fn label<'a>(
        &self,
        label: impl Into<std::borrow::Cow<'a, str>> + 'a,
        size: u16,
    ) -> widget::Text<'a, cosmic::Theme, cosmic::Renderer> {
        ferese_theme::text(label, self.appearance.font).size(size)
    }

    fn screen(&self) -> Element<'_, Message> {
        iced::widget::responsive(move |size| self.screen_content(size)).into()
    }

    fn screen_content(&self, size: iced::Size) -> Element<'_, Message> {
        let avatar_size = (size.height * 0.08).clamp(56., 88.);
        let clock_size = (size.height * 0.06)
            .min(
                size.width
                    * if self.appearance.twelve_hour {
                        0.055
                    } else {
                        0.085
                    },
            )
            .clamp(32., 72.) as u16;
        let margin = if size.width < 640. { 24. } else { 36. };
        let top = (size.height * 0.09).max(24.);
        let a = &self.appearance;
        // The toolkit receives bullets only. Keyboard events edit the locked buffer directly.
        let mut input = widget::text_input("Password", self.password.mask())
            .padding([4, 10])
            .size(14)
            .style(ferese_theme::controls::lock_input(
                a.radius, a.accent, a.panel,
            ))
            .font(a.font)
            .id(widget::Id::new("password"));

        let submit = widget::button::custom(widget::icon::from_name("go-next-symbolic").size(16))
            .class(theme::Button::Icon)
            .padding(4)
            .on_press_maybe(self.state.may_submit().then_some(Message::Submit));
        input = input.trailing_icon(container(submit).padding([0, 4]).into());
        let avatar: Element<'_, Message> = if let Some(photo) = &a.avatar {
            image(photo.clone())
                .width(avatar_size)
                .height(avatar_size)
                .content_fit(iced::ContentFit::Cover)
                .border_radius(avatar_size / 2.)
                .into()
        } else {
            container(line_icon(
                "M16 8a4 4 0 1 1-8 0 4 4 0 0 1 8 0 M4 21v-2a8 8 0 0 1 16 0v2",
                46,
            ))
            .center_x(avatar_size)
            .center_y(avatar_size)
            .class(theme::Container::custom(move |_| {
                let fill = ferese_theme::composite(a.accent.scale_alpha(0.18), a.panel);
                let on = ferese_theme::foreground(fill, a.accent);
                widget::container::Style {
                    background: Some(iced::Background::Color(fill)),
                    text_color: Some(on),
                    icon_color: Some(on),
                    border: iced::Border {
                        shape: cosmic::iced::border::Shape::Continuous,
                        radius: (avatar_size / 2.).into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }
            }))
            .into()
        };
        let identity = column![
            avatar,
            self.label(&self.user, 16).class(iced::Color::WHITE),
            widget::Space::new().height(6),
            input,
            self.label(
                if self.caps_lock {
                    "Caps Lock is on"
                } else {
                    self.status
                },
                12
            )
            .class(iced::Color::WHITE.scale_alpha(0.68)),
        ]
        .spacing(12)
        .align_x(iced::Alignment::Center)
        .width(
            (size.height * 0.23)
                .clamp(200., 240.)
                .min((size.width - margin * 2.).max(160.)),
        );
        let mut clock = column([]).spacing(0).align_x(iced::Alignment::Center);

        if a.show_clock {
            clock = clock.push(
                self.label(&self.clock, clock_size)
                    .font(cosmic::font::Font {
                        weight: cosmic::iced::font::Weight::Light,
                        ..a.font
                    })
                    .class(iced::Color::WHITE),
            );
        }

        if a.show_date {
            clock = clock.push(
                self.label(&self.date, 18)
                    .class(iced::Color::WHITE.scale_alpha(0.72)),
            );
        }

        let clock_height = if a.show_clock {
            clock_size as f32 * 1.3
        } else {
            0.
        } + if a.show_date { 24. } else { 0. };
        let gap = (size.height * 0.44 - top - clock_height).max(24.);
        let foreground = container(
            column![
                container(clock).center_x(Length::Fill),
                widget::Space::new().height(gap),
                container(identity).center_x(Length::Fill),
                widget::Space::new().height(Length::Fill),
                row![
                    row![ferese_symbol(), self.label("Ferese", 13)]
                        .spacing(5)
                        .align_y(iced::Alignment::Center),
                    widget::Space::new().width(Length::Fill),
                    self.label(
                        if self.preview {
                            "Preview · your session is not locked"
                        } else {
                            ""
                        },
                        11
                    )
                ]
                .align_y(iced::Alignment::Center),
            ]
            .height(Length::Fill),
        )
        .padding(iced::Padding {
            top,
            right: margin,
            bottom: margin,
            left: margin,
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .class(theme::Container::custom(move |_| {
            widget::container::Style {
                text_color: Some(iced::Color::WHITE.scale_alpha(0.65)),
                background: Some(iced::Background::Color(iced::Color::from_rgba(
                    0.025, 0.035, 0.06, a.dim,
                ))),
                ..Default::default()
            }
        }));
        let background: Element<'_, Message> = image(a.wallpaper.clone())
            .width(Length::Fill)
            .height(Length::Fill)
            .content_fit(iced::ContentFit::Cover)
            .into();
        widget::mouse_area(iced::widget::stack![background, foreground])
            .on_press(Message::FocusPassword)
            .into()
    }
}

fn line_icon(path: &str, size: u16) -> widget::icon::Icon {
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="{path}" fill="none" stroke="white" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"/></svg>"#
    );
    widget::icon(widget::icon::from_svg_bytes(svg.into_bytes()).symbolic(true)).size(size)
}

fn ferese_symbol() -> widget::icon::Icon {
    widget::icon(widget::icon::from_svg_bytes(ferese_theme::icons::FERESE).symbolic(true)).size(30)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_reenables_input_only_after_the_deadline() {
        let now = Instant::now();
        let mut state = AuthState::Rejected(now);
        assert!(!state.retry_ready(now + Duration::from_millis(1999)));
        assert!(!state.may_submit());
        assert!(state.retry_ready(now + Duration::from_secs(2)));
        assert!(state.may_submit());
        assert!(!state.retry_ready(now + Duration::from_secs(3)));
        state = AuthState::Checking;
        assert!(!state.retry_ready(now + Duration::from_secs(3)));
    }

    #[test]
    fn only_an_active_successful_attempt_unlocks() {
        let mut state = AuthState::Ready;
        assert!(!state.complete(true));
        state = AuthState::Checking;
        assert!(!state.complete(false));
        assert!(!state.may_submit());
        assert!(!state.complete(true));
        state = AuthState::Checking;
        assert!(state.complete(true));
        assert!(!state.complete(true));
    }
    #[test]
    fn password_messages_are_redacted() {
        assert!(
            !format!("{:?}", Message::Input(Zeroizing::new("secret".into()))).contains("secret")
        );
    }
}
