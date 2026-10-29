use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

const PATH: &str = "/org/freedesktop/Notifications";
const INTERFACE: &str = "org.freedesktop.Notifications";
const HISTORY_LIMIT: usize = 100;

#[derive(Clone, Debug)]
pub struct Notice {
    pub id: u32,
    revision: u64,
    pub app: String,
    pub icon: String,
    pub title: String,
    pub body: String,
    pub actions: Vec<(String, String)>,
    pub timeout: Option<Duration>,
    pub critical: bool,
    pub transient: bool,
    pub resident: bool,
    pub live: bool,
    pub unread: bool,
    pub received_at: Instant,
}

#[derive(Clone, Debug)]
pub enum Event {
    Ready,
    Failed(String),
    Notice(Notice),
    Close(u32, u64),
}

enum Command {
    Local(String, String),
    Close(u32, u64, u32),
    Action(u32, u64, String),
}

#[derive(Default)]
struct Registry {
    next: u32,
    live: HashMap<u32, u64>,
    revision: u64,
}

impl Registry {
    fn close(&mut self, id: u32, revision: u64, reason: u32) -> bool {
        match self.live.get(&id) {
            Some(current) if *current == revision => {
                self.live.remove(&id);
                true
            }
            None if reason == 3 => true,
            _ => false,
        }
    }
}

struct Server {
    events: tokio::sync::mpsc::Sender<Event>,
    registry: Arc<Mutex<Registry>>,
    timeout: Arc<std::sync::atomic::AtomicU32>,
}

fn limited(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Server {
    fn get_capabilities(&self) -> Vec<&str> {
        vec!["actions", "body", "icon-static"]
    }

    fn get_server_information(&self) -> (&str, &str, &str, &str) {
        ("Ferese", "Ferese", env!("CARGO_PKG_VERSION"), "1.2")
    }

    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: Vec<String>,
        hints: HashMap<String, zbus::zvariant::OwnedValue>,
        expire_timeout: i32,
    ) -> zbus::fdo::Result<u32> {
        let mut registry = self
            .registry
            .lock()
            .expect("notification ownership registry must remain consistent across callbacks");
        let replacing = registry.live.contains_key(&replaces_id);
        if !replacing && registry.live.len() >= HISTORY_LIMIT {
            return Err(zbus::fdo::Error::LimitsExceeded(
                "Too many active notifications".into(),
            ));
        }
        let id = if replacing {
            replaces_id
        } else {
            loop {
                registry.next = registry.next.wrapping_add(1).max(1);
                if !registry.live.contains_key(&registry.next) {
                    break registry.next;
                }
            }
        };
        let critical = hints.get("urgency").and_then(|v| u8::try_from(v).ok()) == Some(2);
        let timeout = if expire_timeout == 0 || (expire_timeout < 0 && critical) {
            None
        } else {
            Some(Duration::from_millis(if expire_timeout < 0 {
                u64::from(self.timeout.load(std::sync::atomic::Ordering::Relaxed))
            } else {
                expire_timeout as u64
            }))
        };
        let notice = Notice {
            id,
            revision: registry.revision.wrapping_add(1),
            app: limited(app_name, 80),
            icon: limited(app_icon, 512),
            title: limited(summary, 160),
            body: limited(body, 500),
            actions: actions
                .as_chunks::<2>()
                .0
                .iter()
                .take(8)
                .map(|pair| (limited(&pair[0], 128), limited(&pair[1], 40)))
                .collect(),
            timeout,
            critical,
            transient: hints
                .get("transient")
                .and_then(|v| bool::try_from(v).ok())
                .unwrap_or(false),
            resident: hints
                .get("resident")
                .and_then(|v| bool::try_from(v).ok())
                .unwrap_or(false),
            live: true,
            unread: true,
            received_at: Instant::now(),
        };
        let revision = notice.revision;
        self.events
            .try_send(Event::Notice(notice))
            .map_err(|_| zbus::fdo::Error::LimitsExceeded("Notification queue is full".into()))?;
        registry.revision = revision;
        registry.live.insert(id, revision);
        Ok(id)
    }

    fn close_notification(&self, id: u32) -> zbus::fdo::Result<()> {
        let mut registry = self
            .registry
            .lock()
            .expect("notification ownership registry must remain consistent across callbacks");
        let revision = *registry
            .live
            .get(&id)
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("Unknown notification ID".into()))?;
        self.events
            .try_send(Event::Close(id, revision))
            .map_err(|_| zbus::fdo::Error::LimitsExceeded("Notification queue is full".into()))?;
        registry.live.remove(&id);
        Ok(())
    }
}

#[derive(Clone)]
struct Events(Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<Event>>>);

impl std::hash::Hash for Events {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::ptr::hash(Arc::as_ptr(&self.0), state);
    }
}

impl Events {
    fn stream(&self) -> impl cosmic::iced::futures::Stream<Item = Event> + use<> {
        cosmic::iced::futures::stream::unfold(self.0.clone(), |receiver| async move {
            let event = receiver.lock().await.recv().await?;
            Some((event, receiver))
        })
    }
}

pub struct Service {
    timeout: Arc<std::sync::atomic::AtomicU32>,
    events: Events,
    commands: SyncSender<Command>,
}

impl Service {
    fn start(config: &ferese_config::notifications::NotificationConfig) -> Self {
        Self::connect(config, None)
    }

    fn connect(
        config: &ferese_config::notifications::NotificationConfig,
        address: Option<String>,
    ) -> Self {
        let timeout = Arc::new(std::sync::atomic::AtomicU32::new(config.timeout_ms));
        let worker_timeout = timeout.clone();
        let (sender, events) = tokio::sync::mpsc::channel(256);
        let (commands, receiver) = mpsc::sync_channel(256);
        std::thread::spawn(move || {
            let registry = Arc::new(Mutex::new(Registry::default()));
            let server = Server {
                events: sender.clone(),
                registry: registry.clone(),
                timeout: worker_timeout.clone(),
            };
            let connection = match address.as_deref() {
                Some(address) => zbus::blocking::connection::Builder::address(address),
                None => zbus::blocking::connection::Builder::session(),
            }
            .and_then(|builder| builder.serve_at(PATH, server))
            // A preview or test sharing the session bus must not replace the
            // desktop's notification service, or leave it ownerless on exit.
            .map(|builder| {
                builder
                    .allow_name_replacements(false)
                    .replace_existing_names(false)
            })
            .and_then(|builder| builder.name(INTERFACE))
            .and_then(|builder| builder.build());
            let connection = match connection {
                Ok(connection) => connection,
                Err(error) => {
                    let _ = sender.try_send(Event::Failed(error.to_string()));
                    return;
                }
            };
            let _ = sender.try_send(Event::Ready);
            while let Ok(command) = receiver.recv() {
                let result = match command {
                    Command::Local(title, body) => {
                        let server = Server {
                            events: sender.clone(),
                            registry: registry.clone(),
                            timeout: worker_timeout.clone(),
                        };
                        let _ = server.notify(
                            "Ferese",
                            0,
                            "",
                            &title,
                            &body,
                            vec![],
                            HashMap::new(),
                            -1,
                        );
                        continue;
                    }
                    Command::Close(id, revision, reason) => {
                        if !registry.lock().expect("notification ownership registry must remain consistent across callbacks").close(id, revision, reason) {
                            continue;
                        }
                        connection.emit_signal(
                            None::<&str>,
                            PATH,
                            INTERFACE,
                            "NotificationClosed",
                            &(id, reason),
                        )
                    }
                    Command::Action(id, revision, action) => {
                        if registry.lock().expect("notification ownership registry must remain consistent across callbacks").live.get(&id) != Some(&revision) {
                            continue;
                        }
                        connection.emit_signal(
                            None::<&str>,
                            PATH,
                            INTERFACE,
                            "ActionInvoked",
                            &(id, action),
                        )
                    }
                };
                if let Err(error) = result {
                    eprintln!("ferese-shell: notification signal failed: {error}");
                }
            }
        });
        Self {
            timeout,
            events: Events(Arc::new(tokio::sync::Mutex::new(events))),
            commands,
        }
    }
}

struct Toast {
    id: u32,
    motion: super::motion::PopupMotion,
    closing: Option<Instant>,
    remaining: Option<Duration>,
    hovered: bool,
}

pub struct Center {
    pub activity_bridge: bool,
    config: ferese_config::notifications::NotificationConfig,
    service: Service,
    pub ready: bool,
    pub dnd: bool,
    pub history_open: bool,
    pub hovered: Option<u32>,
    pub expanded_apps: HashSet<String>,
    pub entries: VecDeque<Notice>,
    toasts: VecDeque<Toast>,
    last_tick: Instant,
    motion: super::motion::Settings,
}

impl Center {
    pub fn new(
        config: ferese_config::notifications::NotificationConfig,
        motion: super::motion::Settings,
    ) -> Self {
        Self {
            activity_bridge: false,
            service: Service::start(&config),
            ready: false,
            dnd: config.do_not_disturb,
            config,
            history_open: false,
            hovered: None,
            expanded_apps: HashSet::new(),
            entries: VecDeque::new(),
            toasts: VecDeque::new(),
            last_tick: Instant::now(),
            motion,
        }
    }

    pub fn service_error(&self, title: &str, body: &str) {
        let _ = self
            .service
            .commands
            .try_send(Command::Local(title.into(), body.into()));
    }

    pub fn configure_activity_bridge(&mut self, enabled: bool) {
        self.activity_bridge = enabled;
        if enabled {
            self.toasts.clear();
        }
    }
    pub fn configure(&mut self, config: ferese_config::notifications::NotificationConfig) {
        if self.config.do_not_disturb != config.do_not_disturb {
            self.dnd = config.do_not_disturb;
        }
        self.service
            .timeout
            .store(config.timeout_ms, std::sync::atomic::Ordering::Relaxed);
        self.config = config;
        if !self.config.show_popups {
            let ids: Vec<_> = self.toasts.iter().map(|toast| toast.id).collect();
            for id in ids {
                self.close(id, 1);
            }
        }
    }

    pub fn update_motion_settings(&mut self, settings: super::motion::Settings) {
        self.motion = settings;
        for toast in &mut self.toasts {
            toast.motion.update_settings(settings);
        }
    }

    pub fn unread(&self) -> u32 {
        self.entries.iter().filter(|notice| notice.unread).count() as u32
    }

    #[cfg(test)]
    pub fn visible(&self) -> impl Iterator<Item = (&Notice, f32)> {
        self.visible_at(Instant::now())
    }

    fn visible_at(&self, now: Instant) -> impl Iterator<Item = (&Notice, f32)> {
        self.toasts.iter().rev().filter_map(move |toast| {
            let notice = self.entries.iter().find(|notice| notice.id == toast.id)?;
            Some((notice, toast.motion.progress_at(now).clamp(0.0, 1.0)))
        })
    }

    pub fn frame_active(&self, now: Instant) -> bool {
        self.toasts
            .iter()
            .any(|toast| toast.motion.frame_active(now))
    }

    pub fn has_finished_closes(&self, now: Instant) -> bool {
        self.toasts
            .iter()
            .any(|toast| toast.closing.is_some() && !toast.motion.animating_at(now))
    }

    pub fn motion_revision(&self) -> Option<Instant> {
        self.toasts
            .iter()
            .filter_map(|toast| toast.motion.revision())
            .max()
    }

    pub fn popup_groups(&self) -> Vec<(&Notice, f32, usize)> {
        self.popup_groups_at(Instant::now())
    }

    pub fn popup_groups_at(&self, now: Instant) -> Vec<(&Notice, f32, usize)> {
        let mut groups: Vec<(&Notice, f32, usize)> = Vec::new();
        for (notice, opacity) in self.visible_at(now) {
            if let Some(group) = groups
                .iter_mut()
                .find(|(head, _, _)| head.app == notice.app)
            {
                group.2 += 1;
            } else {
                groups.push((notice, opacity, 1));
            }
        }
        groups
    }

    pub fn history_groups(&self) -> Vec<Vec<&Notice>> {
        let mut groups: Vec<Vec<&Notice>> = Vec::new();
        let notice_iter = self.entries.iter().rev();
        for notice in notice_iter {
            if let Some(group) = groups.iter_mut().find(|group| group[0].app == notice.app) {
                group.push(notice);
            } else {
                groups.push(vec![notice]);
            }
        }
        groups
    }

    pub fn toggle_group(&mut self, app: String) {
        if !self.expanded_apps.remove(&app) {
            self.expanded_apps.insert(app);
        }
        self.hovered = None;
    }

    pub fn dismiss_group(&mut self, app: &str) {
        let ids: Vec<_> = self
            .entries
            .iter()
            .filter(|notice| notice.app == app)
            .map(|notice| notice.id)
            .collect();
        for id in ids {
            self.dismiss(id);
        }
        self.expanded_apps.remove(app);
        self.hovered = None;
    }

    pub fn hover(&mut self, id: u32, hovered: bool) {
        self.tick();

        if hovered {
            self.hovered = Some(id);
        } else if self.hovered == Some(id) {
            self.hovered = None;
        }
        // A grouped card represents every toast from its application, so keep
        // the whole stack alive while the user reads it or selects an action.
        let app = self
            .entries
            .iter()
            .find(|notice| notice.id == id)
            .map(|notice| notice.app.clone());
        for toast in &mut self.toasts {
            if self
                .entries
                .iter()
                .any(|notice| notice.id == toast.id && Some(&notice.app) == app.as_ref())
            {
                toast.hovered = hovered;
            }
        }
    }

    fn command(&self, command: Command) {
        if let Err(error) = self.service.commands.try_send(command) {
            eprintln!("ferese-shell: notification command failed: {error}");
        }
    }

    pub fn close(&mut self, id: u32, reason: u32) {
        if let Some(notice) = self.entries.iter_mut().find(|notice| notice.id == id) {
            let was_live = notice.live;
            let revision = notice.revision;
            notice.live = false;
            if was_live || reason == 3 {
                self.command(Command::Close(id, revision, reason));
            }
        }
        if let Some(toast) = self.toasts.iter_mut().find(|toast| toast.id == id)
            && toast.closing.is_none()
        {
            let now = Instant::now();
            toast.closing = Some(now);
            toast.motion.retarget(0.0, now);
        }
    }

    pub fn dismiss(&mut self, id: u32) {
        self.close(id, 2);
        self.toasts.retain(|toast| toast.id != id);
        self.entries.retain(|notice| notice.id != id);
    }

    pub fn invoke(&mut self, id: u32, action: String) {
        if let Some(notice) = self
            .entries
            .iter()
            .find(|notice| notice.id == id && notice.live)
            && notice.actions.iter().any(|(key, _)| key == &action)
        {
            let resident = notice.resident;
            let revision = notice.revision;
            self.command(Command::Action(id, revision, action));
            if !resident {
                self.close(id, 2);
            }
        }
    }

    pub fn toggle_history(&mut self) {
        self.history_open = !self.history_open;
        if self.history_open {
            for notice in &mut self.entries {
                notice.unread = false;
            }
        }
    }

    pub fn clear(&mut self) {
        let ids: Vec<_> = self.entries.iter().map(|notice| notice.id).collect();
        for id in ids {
            self.close(id, 2);
        }
        self.entries.clear();
        self.toasts.clear();
        self.expanded_apps.clear();
        self.hovered = None;
    }

    fn receive(&mut self, mut notice: Notice) {
        let group_hovered = self
            .hovered
            .and_then(|id| self.entries.iter().find(|entry| entry.id == id))
            .is_some_and(|entry| entry.app == notice.app);
        if group_hovered {
            self.hovered = Some(notice.id);
        }
        self.entries.retain(|entry| entry.id != notice.id);
        self.toasts.retain(|toast| toast.id != notice.id);
        if self.entries.len() == HISTORY_LIMIT {
            if let Some(oldest) = self.entries.front() {
                self.close(oldest.id, 1);
            }
            if let Some(oldest) = self.entries.pop_front() {
                self.toasts.retain(|toast| toast.id != oldest.id);
            }
        }
        notice.unread = !self.history_open;
        let show =
            !self.activity_bridge && self.config.show_popups && (!self.dnd || notice.critical);
        if show {
            self.toasts.push_back(Toast {
                id: notice.id,
                motion: {
                    let mut motion = super::motion::PopupMotion::new(self.motion);
                    motion.begin(Instant::now());
                    motion
                },
                closing: None,
                remaining: notice.timeout,
                hovered: group_hovered,
            });
        }
        let id = notice.id;
        self.entries.push_back(notice);
        // Bound visible application groups rather than discarding the fourth
        // message from one app before its count badge can represent the stack.
        while self.popup_groups().len() > 3 {
            let Some(oldest) = self
                .toasts
                .front()
                .and_then(|toast| self.entries.iter().find(|notice| notice.id == toast.id))
            else {
                break;
            };
            let ids: Vec<_> = self
                .entries
                .iter()
                .filter(|notice| notice.app == oldest.app)
                .map(|notice| notice.id)
                .collect();
            for id in &ids {
                self.close(*id, 1);
            }
            self.toasts.retain(|toast| !ids.contains(&toast.id));
        }
        if !show && !self.activity_bridge {
            self.close(id, 1);
        }
    }

    pub fn subscription(&self) -> cosmic::iced::Subscription<Event> {
        cosmic::iced::Subscription::run_with(self.service.events.clone(), Events::stream)
    }

    // Notices presented by Activities still own their timeout, even without a toast.
    fn hidden_deadlines(&self) -> impl Iterator<Item = (u32, Instant)> + '_ {
        self.entries.iter().filter_map(|notice| {
            (notice.live && !self.toasts.iter().any(|toast| toast.id == notice.id))
                .then(|| {
                    notice
                        .timeout
                        .map(|timeout| (notice.id, notice.received_at + timeout))
                })
                .flatten()
        })
    }

    fn expiry_deadline(&self) -> Option<Instant> {
        self.toasts
            .iter()
            .filter(|toast| !toast.hovered && toast.closing.is_none())
            .filter_map(|toast| toast.remaining.map(|remaining| self.last_tick + remaining))
            .chain(self.hidden_deadlines().map(|(_, deadline)| deadline))
            .min()
    }

    pub fn tick_subscription(&self) -> cosmic::iced::Subscription<()> {
        fn deadline_stream(
            deadline: &Instant,
        ) -> impl cosmic::iced::futures::Stream<Item = ()> + use<> {
            let deadline = *deadline;
            cosmic::iced::futures::stream::once(async move {
                tokio::time::sleep(deadline.saturating_duration_since(Instant::now())).await;
            })
        }

        self.expiry_deadline()
            .map_or_else(cosmic::iced::Subscription::none, |deadline| {
                cosmic::iced::Subscription::run_with(deadline, deadline_stream)
            })
    }

    pub fn handle_event(&mut self, event: Event) {
        self.tick();
        match event {
            Event::Ready => self.ready = true,
            Event::Failed(error) => {
                eprintln!("ferese-shell: native notifications unavailable: {error}")
            }
            Event::Notice(notice) => self.receive(notice),
            Event::Close(id, revision) => {
                if self
                    .entries
                    .iter()
                    .any(|notice| notice.id == id && notice.revision == revision)
                {
                    self.close(id, 3);
                } else {
                    self.command(Command::Close(id, revision, 3));
                }
            }
        }
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        let delta = now.saturating_duration_since(self.last_tick);
        self.last_tick = now;
        let mut expired: Vec<_> = self
            .hidden_deadlines()
            .filter_map(|(id, deadline)| (deadline <= now).then_some(id))
            .collect();
        for toast in &mut self.toasts {
            if toast.closing.is_none()
                && !toast.hovered
                && let Some(remaining) = &mut toast.remaining
            {
                *remaining = remaining.saturating_sub(delta);
                if remaining.is_zero() {
                    expired.push(toast.id);
                }
            }
        }
        for id in expired {
            self.close(id, 1);
        }
        self.toasts.retain(|toast| {
            toast.closing.is_none() || (!self.history_open && toast.motion.animating())
        });
        let visible: HashSet<_> = self.toasts.iter().map(|toast| toast.id).collect();
        self.entries
            .retain(|notice| !notice.transient || notice.live || visible.contains(&notice.id));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dismissed_launcher_errors_are_delivered_without_touching_a_new_view() {
        use crate::launcher::Message;
        for reopen in [false, true] {
            let mut shell = crate::tests::shell_with_measured_panel();
            let (sender, receiver) = std::sync::mpsc::sync_channel(4);
            shell.notifications.service.commands = sender;
            let _ = shell.toggle_launcher("test".into());
            let old = shell.launcher.as_ref().unwrap().id;
            let _ = shell.destroy_launcher();
            if reopen {
                let _ = shell.toggle_launcher("test".into());
            }
            let current = shell.launcher.as_ref().map(|l| l.id);
            let _ = shell.update_launcher(Message::Launched(
                old,
                "Editor".into(),
                Err("missing executable".into()),
            ));
            match receiver.try_recv().unwrap() {
                super::Command::Local(title, body) => {
                    assert_eq!(title, "Could not open Editor");
                    assert_eq!(body, "missing executable");
                }
                _ => panic!("expected a visible local notification"),
            }
            assert_eq!(shell.launcher.as_ref().map(|l| l.id), current);
        }
    }

    use ferese_config::notifications::NotificationConfig;

    use super::*;

    fn fixture() -> (Center, std::sync::mpsc::Receiver<Command>) {
        let (_, events) = tokio::sync::mpsc::channel(256);
        let (commands, receiver) = mpsc::sync_channel(256);
        let center = Center {
            activity_bridge: false,
            config: NotificationConfig::default(),
            motion: Default::default(),
            service: Service {
                timeout: Arc::new(std::sync::atomic::AtomicU32::new(6000)),
                events: Events(Arc::new(tokio::sync::Mutex::new(events))),
                commands,
            },
            ready: true,
            dnd: false,
            history_open: false,
            hovered: None,
            expanded_apps: HashSet::new(),
            entries: VecDeque::new(),
            toasts: VecDeque::new(),
            last_tick: Instant::now(),
        };
        (center, receiver)
    }

    fn notice(id: u32) -> Notice {
        Notice {
            id,
            revision: 1,
            app: "Test".into(),
            icon: String::new(),
            title: "Title".into(),
            body: "Body".into(),
            actions: vec![("open".into(), "Open".into())],
            timeout: Some(Duration::from_secs(6)),
            critical: false,
            transient: false,
            resident: false,
            live: true,
            unread: true,
            received_at: Instant::now(),
        }
    }

    #[test]
    fn dnd_immediately_removes_and_restores_a_persistent_bridged_notice() {
        use cosmic::Application;
        let mut app = crate::tests::shell_with_measured_panel();
        app.snapshot.outputs.push(crate::control::OutputSnapshot {
            id: 4,
            name: "test".into(),
            active_workspace: 1,
            focused: true,
        });
        let (center, _commands) = fixture();
        app.notifications = center;
        app.config.activities.sources.notification_bridge = true;
        app.notifications.configure_activity_bridge(true);
        let bar = app.outputs[0].bar;
        let mut persistent = notice(1);
        persistent.timeout = None;
        drop(app.update(crate::Message::NotificationEvent(Event::Notice(persistent))));
        assert!(app.activity_available(bar));
        drop(app.update(crate::Message::Control(crate::status::Action::Dnd(true))));
        assert!(!app.activity_available(bar));
        assert!(
            app.notifications.entries[0].live,
            "DND must not dismiss the notice"
        );
        drop(app.update(crate::Message::Control(crate::status::Action::Dnd(false))));
        assert!(app.activity_available(bar));
    }

    #[test]
    fn bridge_creates_an_activity_and_removes_it_on_expiry_and_dismissal() {
        use cosmic::Application;
        let mut app = crate::tests::shell_with_measured_panel();
        app.snapshot.outputs.push(crate::control::OutputSnapshot {
            id: 4,
            name: "test".into(),
            active_workspace: 1,
            focused: true,
        });
        let (center, _commands) = fixture();
        app.notifications = center;
        app.config.activities.sources.notification_bridge = true;
        app.notifications.configure_activity_bridge(true);
        let bar = app.outputs[0].bar;
        let mut multilingual = notice(1);
        multilingual.title = "通知🛠️".repeat(100);
        multilingual.app = "アプリ".repeat(100);
        drop(app.update(crate::Message::NotificationEvent(Event::Notice(
            multilingual,
        ))));
        assert!(app.activity_available(bar));
        assert!(app.notifications.toasts.is_empty());
        app.notifications.entries[0].received_at = Instant::now() - Duration::from_secs(7);
        drop(app.update(crate::Message::NotificationTick));
        assert!(!app.activity_available(bar));
        drop(app.update(crate::Message::NotificationEvent(Event::Notice(notice(2)))));
        assert!(app.activity_available(bar));
        drop(app.update(crate::Message::DismissNotification(2)));
        assert!(!app.activity_available(bar));
    }

    #[test]
    fn bridged_notice_stays_live_without_toast_until_timeout_or_dismissal() {
        let (mut center, commands) = fixture();
        center.configure_activity_bridge(true);
        center.receive(notice(1));
        assert!(center.toasts.is_empty());
        assert!(center.entries[0].live && center.entries[0].unread);
        assert!(commands.try_recv().is_err());
        assert!(center.expiry_deadline().is_some());
        center.entries[0].received_at = Instant::now() - Duration::from_secs(7);
        center.tick();
        assert!(!center.entries[0].live);
        assert!(matches!(commands.try_recv(), Ok(Command::Close(1, 1, 1))));
        let mut persistent = notice(2);
        persistent.timeout = None;
        center.receive(persistent);
        assert!(center.expiry_deadline().is_none());
        center.dismiss(2);
        assert!(matches!(commands.try_recv(), Ok(Command::Close(2, 1, 2))));
    }

    #[test]
    fn hidden_toast_closes_do_not_wait_for_unavailable_frame_callbacks() {
        let (mut center, _) = fixture();
        center.receive(notice(1));
        center
            .toasts
            .front_mut()
            .unwrap()
            .motion
            .begin(Instant::now());
        center.history_open = true;
        center.close(1, 2);
        center.tick();
        assert!(center.toasts.is_empty());
        assert!(center.expiry_deadline().is_none());
        assert!(!center.frame_active(Instant::now()));
        assert_eq!(center.entries.len(), 1, "history retains the notice");
    }

    #[test]
    fn closing_toast_can_finish_while_another_toast_is_still_moving() {
        let (mut center, _) = fixture();
        center.receive(notice(1));
        let old = Instant::now() - Duration::from_secs(3);
        center.toasts.front_mut().unwrap().motion.retarget(0.0, old);
        center.toasts.front_mut().unwrap().closing = Some(old);
        center.receive(notice(2));
        let now = Instant::now();
        assert!(center.frame_active(now));
        assert!(center.has_finished_closes(now));
        center.tick();
        assert!(!center.has_finished_closes(Instant::now()));
        assert_eq!(center.toasts.len(), 1);
    }

    #[test]
    fn history_groups_expand_and_clear_without_removing_other_apps() {
        let (mut center, commands) = fixture();
        center.receive(notice(1));
        let mut other = notice(2);
        other.app = "Other".into();
        center.receive(other);
        center.receive(notice(3));
        let groups = center.history_groups();
        assert_eq!(
            groups
                .iter()
                .map(|group| group.iter().map(|n| n.id).collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            [vec![3, 1], vec![2]]
        );
        center.toggle_group("Test".into());
        assert!(center.expanded_apps.contains("Test"));
        center.toggle_group("Test".into());
        assert!(!center.expanded_apps.contains("Test"));
        center.dismiss_group("Test");
        assert_eq!(center.entries.iter().map(|n| n.id).collect::<Vec<_>>(), [2]);
        assert_eq!(center.toasts.iter().map(|n| n.id).collect::<Vec<_>>(), [2]);
        let closed: Vec<_> = commands
            .try_iter()
            .map(|command| match command {
                Command::Close(id, _, 2) => id,
                _ => panic!("unexpected action"),
            })
            .collect();
        assert_eq!(closed, [1, 3]);
        center.toggle_group("Other".into());
        center.clear();
        assert!(center.entries.is_empty() && center.expanded_apps.is_empty());
    }

    #[test]
    fn groups_are_newest_first_and_hover_keeps_the_whole_stack_alive() {
        let (mut center, _) = fixture();
        for id in 1..=4 {
            center.receive(notice(id));
        }
        let groups = center.popup_groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0.id, 4);
        assert_eq!(groups[0].2, 4);
        center.hover(4, true);
        center.receive(notice(5));
        assert_eq!(center.hovered, Some(5));
        center.last_tick = Instant::now() - Duration::from_secs(10);
        center.tick();
        assert!(center.entries.iter().all(|notice| notice.live));
        center.hover(5, false);
        center.last_tick = Instant::now() - Duration::from_secs(10);
        center.tick();
        assert!(center.entries.iter().all(|notice| !notice.live));
    }

    #[test]
    fn popup_limit_counts_apps_and_evicts_the_oldest_group() {
        let (mut center, _) = fixture();
        for id in 1..=4 {
            let mut n = notice(id);
            n.app = format!("App {id}");
            center.receive(n);
        }
        let groups = center.popup_groups();
        assert_eq!(groups.iter().map(|g| g.0.id).collect::<Vec<_>>(), [4, 3, 2]);
        assert!(!center.entries[0].live);
        assert_eq!(center.entries.len(), 4);
    }

    #[test]
    fn expiry_deadlines_skip_hovered_and_persistent_toasts() {
        let (mut center, _) = fixture();
        assert!(center.expiry_deadline().is_none());
        center.receive(notice(1));
        assert_eq!(
            center.expiry_deadline(),
            Some(center.last_tick + Duration::from_secs(6))
        );

        center.hover(1, true);
        assert!(center.expiry_deadline().is_none());
        center.hover(1, false);
        assert!(center.expiry_deadline().is_some());

        center.toasts[0].remaining = None;
        assert!(center.expiry_deadline().is_none());
    }

    #[test]
    fn hover_pauses_expiry_and_replacements_reset_the_timer() {
        let (mut center, commands) = fixture();
        center.receive(notice(1));
        center.hover(1, true);
        center.last_tick = Instant::now() - Duration::from_secs(10);
        center.tick();
        assert!(center.entries[0].live);
        center.hover(1, false);
        center.receive(notice(1));
        assert_eq!(center.toasts.len(), 1);
        assert_eq!(center.toasts[0].remaining, Some(Duration::from_secs(6)));
        center.last_tick = Instant::now() - Duration::from_secs(10);
        center.tick();
        assert!(!center.entries[0].live);
        assert!(matches!(
            commands.try_recv().unwrap(),
            Command::Close(1, 1, 1)
        ));
    }

    #[test]
    fn dnd_keeps_history_but_only_critical_notifications_pop_up() {
        let (mut center, _) = fixture();
        center.dnd = true;
        center.receive(notice(1));
        let mut critical = notice(2);
        critical.critical = true;
        critical.timeout = None;
        center.receive(critical);
        assert_eq!(center.entries.len(), 2);
        assert_eq!(center.toasts.len(), 1);
        assert_eq!(center.toasts[0].id, 2);
        center.toggle_history();
        assert_eq!(center.unread(), 0);
        center.clear();
        assert!(center.entries.is_empty() && center.toasts.is_empty());
    }

    #[test]
    fn history_is_bounded_and_transient_messages_are_not_retained() {
        let (mut center, _commands) = fixture();
        for id in 1..=150 {
            center.receive(notice(id));
        }
        assert_eq!(center.entries.len(), HISTORY_LIMIT);
        assert!(center.popup_groups().len() <= 3);
        let mut transient = notice(151);
        transient.transient = true;
        center.receive(transient);
        center.close(151, 1);
        center.toasts.clear();
        center.tick();
        assert!(!center.entries.iter().any(|notice| notice.id == 151));
    }

    #[test]
    fn actions_are_validated_and_resident_messages_remain_live() {
        let (mut center, commands) = fixture();
        let mut notification = notice(1);
        notification.resident = true;
        center.receive(notification);
        center.invoke(1, "invalid".into());
        assert!(commands.try_recv().is_err());
        center.invoke(1, "open".into());
        assert!(matches!(
            commands.try_recv().unwrap(),
            Command::Action(1, 1, _)
        ));
        assert!(center.entries[0].live);
    }

    #[test]
    fn an_old_timeout_cannot_close_a_new_replacement() {
        let mut registry = Registry::default();
        registry.live.insert(7, 2);
        assert!(!registry.close(7, 1, 1));
        assert_eq!(registry.live.get(&7), Some(&2));
        assert!(registry.close(7, 2, 1));
    }

    #[test]
    fn config_changes_update_timeout_without_resetting_a_runtime_dnd_toggle() {
        let (mut center, _) = fixture();
        center.dnd = true;
        center.configure(NotificationConfig {
            timeout_ms: 10000,
            ..Default::default()
        });
        assert!(center.dnd);
        assert_eq!(
            center
                .service
                .timeout
                .load(std::sync::atomic::Ordering::Relaxed),
            10000
        );
        center.receive(notice(1));
        center.configure(NotificationConfig {
            show_popups: false,
            ..Default::default()
        });
        assert!(center.visible().all(|(notice, _)| !notice.live));
    }

    #[test]
    fn real_dbus_delivery_replacement_and_close_signal() {
        use std::io::{BufRead, BufReader};
        use std::process::{Command as ProcessCommand, Stdio};
        struct Daemon(std::process::Child);
        impl Drop for Daemon {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let mut daemon = Daemon(
            ProcessCommand::new("dbus-daemon")
                .args(["--session", "--nofork", "--nopidfile", "--print-address=1"])
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut address = String::new();
        BufReader::new(daemon.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let config = NotificationConfig {
            timeout_ms: 11000,
            ..Default::default()
        };
        let service = Service::connect(&config, Some(address.trim().to_owned()));
        let received = |service: &Service| {
            let end = Instant::now() + Duration::from_secs(3);
            loop {
                if let Ok(event) = service.events.0.blocking_lock().try_recv() {
                    break event;
                }
                assert!(Instant::now() < end, "notification event timed out");
                std::thread::sleep(Duration::from_millis(5));
            }
        };
        assert!(matches!(received(&service), Event::Ready));
        let connection = zbus::blocking::connection::Builder::address(address.trim())
            .unwrap()
            .method_timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let proxy = zbus::blocking::Proxy::new(&connection, INTERFACE, PATH, INTERFACE).unwrap();
        let info: (String, String, String, String) =
            proxy.call("GetServerInformation", &()).unwrap();
        assert_eq!(info.0, "Ferese");
        // A second shell on this bus must fail to claim the name without
        // displacing the running desktop's notification service.
        let second = Service::connect(&config, Some(address.trim().to_owned()));
        assert!(matches!(received(&second), Event::Failed(_)));
        drop(second);
        let info: (String, String, String, String) =
            proxy.call("GetServerInformation", &()).unwrap();
        assert_eq!(info.0, "Ferese");
        // Older shells use zbus's default replacement flags. They must not be
        // able to steal the name either.
        let replacement = zbus::blocking::connection::Builder::address(address.trim())
            .unwrap()
            .name(INTERFACE)
            .unwrap()
            .build();
        assert!(matches!(replacement, Err(zbus::Error::NameTaken)));
        let notify = |replace| -> u32 {
            proxy
                .call(
                    "Notify",
                    &(
                        "Test",
                        replace,
                        "",
                        "Title",
                        "Body",
                        vec!["open", "Open"],
                        HashMap::<String, zbus::zvariant::OwnedValue>::new(),
                        -1_i32,
                    ),
                )
                .unwrap()
        };
        let id = notify(0_u32);
        let Event::Notice(first) = received(&service) else {
            panic!("Expected notification");
        };
        assert_eq!(first.id, id);
        assert_eq!(first.timeout, Some(Duration::from_secs(11)));
        assert_eq!(notify(id), id);
        let Event::Notice(replacement) = received(&service) else {
            panic!("Expected replacement");
        };
        assert_ne!(first.revision, replacement.revision);
        let mut signals = proxy.receive_signal("NotificationClosed").unwrap();
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let signal = signals.next().unwrap();
            sender
                .send(signal.body().deserialize::<(u32, u32)>().unwrap())
                .unwrap();
        });
        proxy.call::<_, _, ()>("CloseNotification", &(id,)).unwrap();
        let Event::Close(closed, revision) = received(&service) else {
            panic!("Expected close");
        };
        service
            .commands
            .send(Command::Close(closed, revision, 3))
            .unwrap();
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(3)).unwrap(),
            (id, 3)
        );
        worker.join().unwrap();
        assert!(proxy.call::<_, _, ()>("CloseNotification", &(id,)).is_err());
    }
}
