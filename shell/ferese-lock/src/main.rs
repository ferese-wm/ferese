mod appearance;
mod auth;
mod auth_worker;
#[cfg(test)]
mod lock_tests;
mod password;
mod password_field;
#[cfg(test)]
mod refusal_tests;
mod runtime;
mod screen;
mod wallpaper;

use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use cosmic::app::{Core, Settings, Task};
use cosmic::iced::event::{PlatformSpecific, wayland};
use cosmic::iced::platform_specific::shell::commands::session_lock;
use cosmic::iced::{Event, Subscription, event, window};
use cosmic::{Element, iced, widget};
use wayland_client::protocol::wl_output::WlOutput;
use zeroize::Zeroizing;

use appearance::Appearance;
use auth::Outcome;
use password::Password;

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--authenticate"] {
        std::process::exit(auth_worker::run()?);
    }
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
    Authenticated(Outcome),
    Tick,
    RetryReady,
    FocusPassword,
    KeyboardLayout(Option<String>),
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

    fn may_edit(&self) -> bool {
        matches!(self, Self::Ready | Self::Rejected(_))
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

    fn complete(&mut self, outcome: Outcome) -> bool {
        if *self != Self::Checking {
            return false;
        }
        let success = outcome == Outcome::Accepted;
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
    keyboard_layout: Option<String>,
    layout_pending: bool,
    layout_dirty: bool,
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
                status: "Enter your password to unlock",
                outputs: HashMap::new(),
                started: false,
                confirmation: Default::default(),
                caps_lock: false,
                keyboard_layout: None,
                layout_pending: false,
                layout_dirty: false,
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
            Message::KeyboardLayout(name) => {
                self.layout_pending = false;
                self.keyboard_layout = name;
                if std::mem::take(&mut self.layout_dirty) {
                    return self.refresh_keyboard_layout();
                }
            }
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
                self.appearance.high_contrast = snapshot.presented.accessibility.increase_contrast;
                self.appearance.reduce_transparency =
                    snapshot.presented.accessibility.reduce_transparency;
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
                if self.state.may_edit() {
                    return focus();
                }
            }
            Message::Input(value) => {
                if self.state.may_edit() && !value.is_empty() && self.password.push_str(&value) {
                    self.input_at = Some(Instant::now());
                    if self.state.may_submit() {
                        self.status = "Press Enter to unlock";
                    }
                }
            }
            Message::Backspace => {
                if self.state.may_edit() {
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

                if !self.can_submit() {
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
                return Task::perform(auth_worker::authenticate(password), |outcome| {
                    cosmic::Action::App(Message::Authenticated(outcome))
                });
            }
            Message::Authenticated(outcome) => {
                if self.state != AuthState::Checking {
                    return Task::none();
                }
                self.status = outcome.message();
                if self.state.complete(outcome) && !self.preview {
                    return session_lock::unlock();
                }
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
    fn refresh_keyboard_layout(&mut self) -> Task<Message> {
        if self.layout_pending {
            self.layout_dirty = true;
            return Task::none();
        }
        self.layout_pending = true;
        cosmic::task::future(async {
            let name = tokio::task::spawn_blocking(|| {
                let mut connection = ferese_ipc::Connection::connect().ok()?;
                let value = connection
                    .call("get-keyboard-layout", serde_json::json!({}))
                    .ok()?;
                value["name"].as_str().map(str::to_owned)
            })
            .await
            .ok()
            .flatten();
            Message::KeyboardLayout(name)
        })
    }

    fn can_submit(&self) -> bool {
        (self.preview || self.confirmation.confirmed())
            && self.state.may_submit()
            && !self.password.is_empty()
    }

    fn handle_event(&mut self, event: Event) -> Task<Message> {
        match event {
            Event::Keyboard(iced::keyboard::Event::ModifiersChanged(modifiers)) => {
                self.caps_lock = modifiers.contains(iced::keyboard::Modifiers::CAPS_LOCK);
                return self.refresh_keyboard_layout();
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
                wayland::SessionLockEvent::Focused(..) => {
                    return Task::batch([focus(), self.refresh_keyboard_layout()]);
                }
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
                return Task::batch([focus(), self.refresh_keyboard_layout()]);
            }
            _ => {}
        }

        Task::none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_reenables_submission_only_after_the_deadline() {
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
        assert!(!state.complete(Outcome::Accepted));
        state = AuthState::Checking;
        assert!(!state.complete(Outcome::Rejected));
        assert!(!state.may_submit());
        assert!(!state.complete(Outcome::Accepted));
        state = AuthState::Checking;
        assert!(state.complete(Outcome::Accepted));
        assert!(!state.complete(Outcome::Accepted));
    }
    #[test]
    fn password_messages_are_redacted() {
        assert!(
            !format!("{:?}", Message::Input(Zeroizing::new("secret".into()))).contains("secret")
        );
    }
}
