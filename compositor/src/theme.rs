use std::collections::{HashMap, HashSet};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{Interest, Mode as PollMode, PostAction};
use ferese_config::Document;
use ferese_config::theme::{Candidate, Mode, TRANSITION_MS, resolve_with_context};
use ferese_ipc::{
    Response,
    theme::{SCHEMA_VERSION, Snapshot},
};
use jiff::Timestamp;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde_json::{Value, json};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::output::Output;

use crate::config::Config;
use crate::wallpaper::{WallpaperConfig, WallpaperState};
use crate::{Ferese, RuntimeConfig};

const DEBOUNCE: Duration = Duration::from_millis(150);
const WALLPAPER_TIMEOUT: Duration = Duration::from_secs(2);

fn transition_progress(elapsed: Duration, duration: Duration) -> f64 {
    if duration.is_zero() {
        1.0
    } else {
        (elapsed.as_secs_f64() / duration.as_secs_f64()).clamp(0.0, 1.0)
    }
}

pub(crate) fn read_source(path: &Path) -> Result<String, String> {
    let mut source = String::new();
    std::fs::File::open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .take(60 * 1024 + 1)
        .read_to_string(&mut source)
        .map_err(|e| e.to_string())?;
    if source.len() > 60 * 1024 {
        return Err(format!("{} exceeds 60 KiB", path.display()));
    }
    Ok(source)
}

pub(crate) fn prepare(source: &str, directory: &Path) -> Result<(Config, RuntimeConfig, Candidate), String> {
    let document = Document::parse(source).map_err(|e| e.to_string())?;
    prepare_document(&document, directory)
}

pub(crate) fn prepare_document(
    document: &Document,
    directory: &Path,
) -> Result<(Config, RuntimeConfig, Candidate), String> {
    let candidate = resolve_with_context(document, directory, Timestamp::now(), &auto_context(), read_source)?;
    let config: Config =
        serde_json::from_value(document.with_theme(&candidate.theme).value().clone()).map_err(|e| e.to_string())?;
    let runtime = config.runtime_config().map_err(|e| e.to_string())?;
    Ok((config, runtime, candidate))
}

pub(crate) fn preview(args: &Value) -> Result<Value, String> {
    let source = args["source"]
        .as_str()
        .filter(|source| source.len() <= 60 * 1024)
        .ok_or("Missing candidate configuration")?;
    let directory = args["directory"]
        .as_str()
        .map(PathBuf::from)
        .or_else(|| crate::config::config_path().and_then(|path| path.parent().map(ToOwned::to_owned)))
        .unwrap_or_else(|| PathBuf::from("."));
    let document = Document::parse(source).map_err(|e| e.to_string())?;
    let candidate = resolve_with_context(&document, &directory, Timestamp::now(), &auto_context(), read_source)?;
    Ok(
        json!({"theme": candidate.theme, "warnings": candidate.warnings, "families": candidate.families, "fallback_note": candidate.fallback_note}),
    )
}

fn system_appearance_value(value: &str) -> Option<ferese_config::theme::Appearance> {
    use ferese_config::theme::Appearance;
    if value.contains("prefer-dark") {
        Some(Appearance::Dark)
    } else if value.contains("prefer-light") || value.contains("default") {
        Some(Appearance::Light)
    } else {
        None
    }
}

static SYSTEM_APPEARANCE: std::sync::OnceLock<Mutex<Option<ferese_config::theme::Appearance>>> =
    std::sync::OnceLock::new();

fn auto_context() -> ferese_config::theme::AutoContext {
    let appearance = SYSTEM_APPEARANCE.get_or_init(|| {
        let value = crate::process::command("gsettings")
            .args(["get", "org.gnome.desktop.interface", "color-scheme"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| system_appearance_value(&String::from_utf8_lossy(&output.stdout)));
        Mutex::new(value)
    });
    ferese_config::theme::AutoContext {
        system: *appearance.lock().unwrap(),
    }
}

struct Pending {
    candidate: Candidate,
    wallpaper: Option<WallpaperState>,
    ready: HashSet<Output>,
    deadline: Instant,
}

struct Transition {
    from: ferese_config::theme::ResolvedTheme,
    started: Instant,
    previous_wallpaper: Option<WallpaperState>,
}

pub(crate) struct LiveTheme {
    pub(crate) mode: Mode,
    pub(crate) theme: ferese_config::theme::ResolvedTheme,
    pub(crate) presented: ferese_config::theme::ResolvedTheme,
    pub(crate) warnings: Vec<String>,
    pub(crate) error: Option<String>,
    pub(crate) families: Vec<ferese_config::families::Family>,
    pub(crate) fallback_note: Option<String>,
}

impl Default for LiveTheme {
    fn default() -> Self {
        let theme = ferese_config::theme::default_theme();
        Self {
            mode: Mode::Dark,
            presented: theme.clone(),
            theme,
            warnings: Vec::new(),
            error: None,
            families: ferese_config::families::builtins(),
            fallback_note: None,
        }
    }
}

#[derive(Default)]
pub(crate) struct Engine {
    pub(crate) live: LiveTheme,
    pub(crate) revision: u64,
    waiters: HashMap<u64, (u64, mpsc::SyncSender<Response>)>,
    watcher: Option<RecommendedWatcher>,
    watched_directories: HashSet<PathBuf>,
    active_files: Vec<PathBuf>,
    paths: Arc<Mutex<HashSet<PathBuf>>>,
    events: Option<mpsc::Receiver<()>>,
    dirty: Option<Instant>,
    timer_pending: bool,
    clock: Option<OwnedFd>,
    pending: Option<Pending>,
    transition: Option<Transition>,
    last_frame: Option<Instant>,
}

impl Engine {
    pub(crate) fn matches_files(&self, files: &[PathBuf]) -> bool {
        self.active_files == files
    }

    pub(crate) fn watch(&mut self, owner: u64, id: u64, since: u64, response: mpsc::SyncSender<Response>) {
        if since != self.revision {
            let _ = response.try_send(Response::success(id, self.value()));
        } else {
            self.waiters.insert(owner, (id, response));
        }
    }

    pub(crate) fn remove(&mut self, owner: u64) {
        self.waiters.remove(&owner);
    }

    pub(crate) fn value(&self) -> Value {
        let snapshot = Snapshot {
            version: SCHEMA_VERSION,
            revision: self.revision,
            mode: self.live.mode,
            theme: self.live.theme.clone(),
            presented: self.live.presented.clone(),
            warnings: self.live.warnings.clone(),
            error: self.live.error.clone(),
            families: self.live.families.clone(),
            fallback_note: self.live.fallback_note.clone(),
        };
        serde_json::to_value(snapshot).expect("serializable theme snapshot")
    }

    fn publish(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        let value = self.value();
        for (_, (id, response)) in self.waiters.drain() {
            let _ = response.try_send(Response::success(id, value.clone()));
        }
    }

    pub(crate) fn reject(&mut self, error: String) {
        if self.live.error.as_ref() != Some(&error) {
            tracing::warn!(%error, "theme reload rejected; retaining active theme");
            self.live.error = Some(error);
            self.publish();
        }
    }

    pub(crate) fn watch_files(&mut self, files: &[PathBuf]) {
        let mut targets = HashSet::from([PathBuf::from("/etc/localtime")]);
        if let Some(path) = crate::config::config_path() {
            targets.insert(path);
        }
        for path in self.active_files.iter().chain(files) {
            let path = if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
                parent.canonicalize().unwrap_or_else(|_| parent.to_owned()).join(name)
            } else {
                path.clone()
            };
            if let Ok(target) = path.canonicalize() {
                targets.insert(target);
            }
            targets.insert(path);
        }
        let mut directories: HashSet<_> = targets
            .iter()
            .map(|path| {
                let mut directory = path.parent().unwrap_or(Path::new("."));
                while !directory.is_dir() {
                    directory = directory.parent().unwrap_or(Path::new("/"));
                }
                directory.to_owned()
            })
            .collect();
        *self.paths.lock().unwrap() = targets;
        if let Some(watcher) = &mut self.watcher {
            for directory in self.watched_directories.difference(&directories) {
                let _ = watcher.unwatch(directory);
            }
            let mut failed = Vec::new();
            for directory in directories.difference(&self.watched_directories) {
                let mode = if directory == Path::new("/etc") {
                    RecursiveMode::NonRecursive
                } else {
                    RecursiveMode::Recursive
                };
                if let Err(error) = watcher.watch(directory, mode) {
                    tracing::warn!(%error, path = %directory.display(), "cannot watch theme directory");
                    failed.push(directory.clone());
                }
            }
            for directory in failed {
                directories.remove(&directory);
            }
        }
        self.watched_directories = directories;
    }

    pub(crate) fn arm_clock(&self, next: Option<Timestamp>) -> io::Result<()> {
        let Some(fd) = &self.clock else { return Ok(()) };
        let mut timer: libc::itimerspec = unsafe { std::mem::zeroed() };
        if let Some(next) = next {
            timer.it_value.tv_sec = next.as_second();
            timer.it_value.tv_nsec = i64::from(next.subsec_nanosecond());
        }
        let result = unsafe {
            libc::timerfd_settime(
                fd.as_raw_fd(),
                libc::TFD_TIMER_ABSTIME | libc::TFD_TIMER_CANCEL_ON_SET,
                &timer,
                std::ptr::null_mut(),
            )
        };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

pub(crate) fn init(
    event_loop: &mut calloop::EventLoop<'static, Ferese>,
    state: &mut Ferese,
    candidate: Candidate,
) -> Result<(), Box<dyn std::error::Error>> {
    let clock = unsafe { libc::timerfd_create(libc::CLOCK_REALTIME, libc::TFD_CLOEXEC | libc::TFD_NONBLOCK) };
    if clock < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let clock = unsafe { OwnedFd::from_raw_fd(clock) };
    let source = clock.try_clone()?;
    state.theme_engine.clock = Some(clock);
    event_loop
        .handle()
        .insert_source(Generic::new(source, Interest::READ, PollMode::Level), |_, _, state| {
            if let Some(clock) = &state.theme_engine.clock {
                let mut expirations = 0u64;
                unsafe { libc::read(clock.as_raw_fd(), (&mut expirations as *mut u64).cast(), 8) };
            }
            state.refresh_theme();
            Ok(PostAction::Continue)
        })?;
    let (sender, events) = mpsc::sync_channel(1);
    let paths = state.theme_engine.paths.clone();
    if let Some(config) = crate::config::config_path() {
        paths.lock().unwrap().insert(config);
    }
    paths.lock().unwrap().insert(PathBuf::from("/etc/localtime"));
    let wakeup = state.loop_signal.clone();
    let system_sender = sender.clone();
    let system_wakeup = wakeup.clone();
    std::thread::Builder::new()
        .name("ferese-system-appearance".into())
        .spawn(move || {
            use std::io::BufRead;
            use std::os::unix::process::CommandExt;
            let mut command = crate::process::command("gsettings");
            unsafe {
                command.pre_exec(|| {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let monitor = command
                .args(["monitor", "org.gnome.desktop.interface", "color-scheme"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn();
            if let Ok(mut monitor) = monitor {
                if let Ok(output) = crate::process::command("gsettings")
                    .args(["get", "org.gnome.desktop.interface", "color-scheme"])
                    .output()
                {
                    if let Some(state) = SYSTEM_APPEARANCE.get() {
                        *state.lock().unwrap() = system_appearance_value(&String::from_utf8_lossy(&output.stdout));
                    }
                    let _ = system_sender.try_send(());
                    system_wakeup.wakeup();
                }
                if let Some(output) = monitor.stdout.take() {
                    for line in std::io::BufReader::new(output).lines().map_while(Result::ok) {
                        let value = system_appearance_value(&line);
                        if let Some(state) = SYSTEM_APPEARANCE.get() {
                            *state.lock().unwrap() = value;
                        }
                        let _ = system_sender.try_send(());
                        system_wakeup.wakeup();
                    }
                }
                let _ = monitor.wait();
            }
        })?;
    let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event
            && !event.kind.is_access()
            && event.paths.iter().any(|path| {
                paths
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|target| target == path || target.starts_with(path))
            })
        {
            let _ = sender.try_send(());
            wakeup.wakeup();
        }
    })?;
    state.theme_engine.watcher = Some(watcher);
    state.theme_engine.events = Some(events);
    state.theme_engine.active_files = candidate.files.clone();
    state.theme_engine.watch_files(&candidate.files);
    state.theme_engine.arm_clock(candidate.next_transition)?;
    state.theme_engine.live.families = candidate.families;
    state.theme_engine.live.fallback_note = candidate.fallback_note;
    state.theme_engine.live.mode = candidate.policy.mode;
    state.theme_engine.live.theme = candidate.theme.clone();
    state.theme_engine.live.presented = candidate.theme;
    state.theme_engine.live.warnings = candidate.warnings;
    state.theme_engine.publish();
    Ok(())
}

impl Ferese {
    pub(crate) fn refresh_theme(&mut self) {
        if let Err(error) = self.queue_config_reload(true, None) {
            self.theme_engine.reject(error);
        }
    }

    pub(crate) fn accept_theme(&mut self, candidate: Candidate, wallpaper: WallpaperConfig) {
        self.theme_engine.active_files = candidate.files.clone();
        self.theme_engine.watch_files(&candidate.files);
        if let Err(error) = self.theme_engine.arm_clock(candidate.next_transition) {
            tracing::warn!(%error, "cannot arm appearance schedule");
        }
        if candidate.theme == self.theme_engine.live.theme && self.theme_engine.pending.is_none() {
            let changed = self.theme_engine.live.mode != candidate.policy.mode
                || self.theme_engine.live.warnings != candidate.warnings
                || self.theme_engine.live.error.is_some()
                || self.theme_engine.live.families != candidate.families
                || self.theme_engine.live.fallback_note != candidate.fallback_note;
            self.theme_engine.live.families = candidate.families;
            self.theme_engine.live.fallback_note = candidate.fallback_note;
            self.theme_engine.live.mode = candidate.policy.mode;
            self.theme_engine.live.warnings = candidate.warnings;
            self.theme_engine.live.error = None;
            if changed {
                self.theme_engine.publish();
            }
            return;
        }
        if let Some(pending) = &mut self.theme_engine.pending
            && pending
                .wallpaper
                .as_ref()
                .is_some_and(|state| state.configuration() == &wallpaper)
        {
            pending.candidate = candidate;
            self.poll_theme();
            return;
        }
        let wallpaper = (self.wallpaper.configuration() != &wallpaper).then(|| self.wallpaper.replacement(wallpaper));
        self.theme_engine.pending = Some(Pending {
            candidate,
            wallpaper,
            ready: HashSet::new(),
            deadline: Instant::now() + WALLPAPER_TIMEOUT,
        });
        self.poll_theme();
    }

    pub(crate) fn prepare_theme_wallpaper(&mut self, renderer: &mut GlesRenderer, output: &Output) {
        if let Some(pending) = &mut self.theme_engine.pending
            && let Some(wallpaper) = &mut pending.wallpaper
        {
            wallpaper.poll();
            if wallpaper.element(renderer, output).is_some() || wallpaper.configuration().path.is_none() {
                pending.ready.insert(output.clone());
            }
        }
    }

    pub(crate) fn previous_theme_wallpaper(
        &mut self,
        renderer: &mut GlesRenderer,
        output: &Output,
    ) -> Option<crate::presentation::NativeTextureElement> {
        let transition = self.theme_engine.transition.as_mut()?;
        transition.previous_wallpaper.as_mut()?.element(renderer, output)
    }

    pub(crate) fn theme_progress(&self) -> f32 {
        self.theme_engine
            .transition
            .as_ref()
            .filter(|transition| transition.previous_wallpaper.is_some())
            .map_or(1., |transition| {
                transition_progress(transition.started.elapsed(), self.theme_transition_duration()) as f32
            })
    }

    fn theme_transition_duration(&self) -> Duration {
        if self.theme_engine.live.theme.reduced_motion {
            Duration::ZERO
        } else {
            self.animation_duration(Duration::from_millis(TRANSITION_MS))
        }
    }

    pub(crate) fn poll_theme(&mut self) {
        let now = Instant::now();
        if self
            .theme_engine
            .events
            .as_ref()
            .is_some_and(|events| events.try_iter().next().is_some())
        {
            self.theme_engine.dirty = Some(now + DEBOUNCE);
        }
        if self.theme_engine.dirty.is_some_and(|deadline| now >= deadline) {
            self.theme_engine.dirty = None;
            self.refresh_theme();
        }
        let mut start = false;
        if let Some(pending) = &mut self.theme_engine.pending {
            if let Some(wallpaper) = &mut pending.wallpaper {
                wallpaper.poll();
            }
            let ready =
                pending.wallpaper.is_none() || self.space.outputs().all(|output| pending.ready.contains(output));
            let failed = pending.wallpaper.as_ref().is_some_and(WallpaperState::failed);
            start = ready || failed || now >= pending.deadline;
            if start && !ready {
                pending.wallpaper = None;
                pending
                    .candidate
                    .warnings
                    .push("Wallpaper unavailable; retained previous image".into());
                tracing::warn!("theme wallpaper was not ready; retaining previous image");
            }
        }
        if start {
            let pending = self.theme_engine.pending.take().unwrap();
            let from = self.theme_engine.live.presented.clone();
            let previous_wallpaper = pending
                .wallpaper
                .map(|next| std::mem::replace(&mut self.wallpaper, next));
            self.theme_engine.live.families = pending.candidate.families;
            self.theme_engine.live.fallback_note = pending.candidate.fallback_note;
            self.theme_engine.live.mode = pending.candidate.policy.mode;
            self.theme_engine.live.theme = pending.candidate.theme;
            self.theme_engine.live.warnings = pending.candidate.warnings;
            self.theme_engine.live.error = None;
            self.theme_engine.transition = Some(Transition {
                from,
                started: now,
                previous_wallpaper,
            });
            self.theme_engine.last_frame = None;
            // Publish the target appearance before drawing transitional tokens.
            self.theme_engine.publish();
        }
        let frame_due = self
            .theme_engine
            .last_frame
            .is_none_or(|last| now.saturating_duration_since(last) >= Duration::from_millis(16));
        let duration = self.theme_transition_duration();
        if frame_due && let Some(transition) = &self.theme_engine.transition {
            self.theme_engine.last_frame = Some(now);
            let progress = transition_progress(now.saturating_duration_since(transition.started), duration);
            let target = &self.theme_engine.live.theme;
            let complete = progress >= 1.;
            let presented = if complete {
                target.clone()
            } else {
                transition.from.transition(target, progress)
            };
            self.theme_settings = Config::resolved_theme_settings(&presented).expect("validated resolved theme");
            self.theme_engine.live.presented = presented;
            self.theme_engine.publish();

            if complete {
                self.theme_engine.transition = None;
            }
            crate::backends::direct::render_all(self);
        } else if frame_due && self.theme_engine.pending.is_some() {
            self.theme_engine.last_frame = Some(now);
            crate::backends::direct::render_all(self);
        }
        if !self.theme_engine.timer_pending {
            let deadline = if self.theme_engine.transition.is_some() || self.theme_engine.pending.is_some() {
                Some(now + Duration::from_millis(16))
            } else {
                self.theme_engine.dirty
            };
            if let Some(deadline) = deadline {
                self.theme_engine.timer_pending = true;
                if let Err(error) = self
                    .loop_handle
                    .insert_source(Timer::from_deadline(deadline), |_, _, state| {
                        state.theme_engine.timer_pending = false;
                        state.poll_theme();
                        TimeoutAction::Drop
                    })
                {
                    self.theme_engine.timer_pending = false;
                    tracing::warn!(%error, "cannot schedule theme update");
                }
            }
        }
    }

    pub(crate) fn set_theme_mode(&mut self, mode: Mode) -> Result<Value, String> {
        let path = crate::config::config_path().ok_or("No configuration directory")?;
        let original = read_source(&path)?;
        let mut document = Document::parse(&original).map_err(|e| e.to_string())?;
        document.set("theme.mode", json!(mode)).map_err(|e| e.to_string())?;
        let source = document.to_string();
        prepare(&source, path.parent().unwrap_or(Path::new(".")))?;
        let temporary = path.with_extension(format!("kdl.{}.tmp", std::process::id()));
        let result = (|| -> Result<(), String> {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|e| e.to_string())?;
            if let Ok(metadata) = std::fs::metadata(&path) {
                file.set_permissions(metadata.permissions())
                    .map_err(|e| e.to_string())?;
            }
            file.write_all(source.as_bytes()).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            if read_source(&path)? != original {
                return Err("Configuration changed while updating the mode; please try again".into());
            }
            std::fs::rename(&temporary, &path).map_err(|e| e.to_string())?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result?;
        self.reload_config_source(source)?;
        Ok(self.theme_engine.value())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_projects_live_values_and_preserves_revision_semantics() {
        let mut engine = Engine::default();
        let original = engine.value();
        engine.live.mode = Mode::Light;
        engine.live.theme = ferese_config::theme::resolve(
            &Document::parse("theme { mode \"light\"; }\n").unwrap(),
            Path::new("/unused"),
            "2026-10-02T12:00:00Z".parse().unwrap(),
            |_| unreachable!(),
        )
        .unwrap()
        .theme;
        // The target can advance while presented pixels still describe the old theme.
        let (send, receive) = mpsc::sync_channel(1);
        engine.watch(1, 42, 0, send);
        assert!(receive.try_recv().is_err());
        engine.publish();
        let response = receive.try_recv().unwrap();
        assert_eq!(response.id, 42);
        let value = response.result.unwrap();
        assert_eq!(value["version"], SCHEMA_VERSION);
        assert_eq!(value["revision"], 1);
        assert_eq!(value["mode"], "light");
        assert_eq!(value["theme"]["appearance"], "light");
        assert_eq!(value["presented"], original["presented"]);
        engine.reject("invalid configuration".into());
        assert_eq!(engine.revision, 2);
        assert_eq!(engine.value()["theme"], value["theme"]);
        engine.reject("invalid configuration".into());
        assert_eq!(engine.revision, 2);
    }

    #[test]
    fn theme_and_wallpaper_share_scaled_progress_and_instant_completion() {
        let elapsed = Duration::from_millis(50);
        assert_eq!(transition_progress(elapsed, Duration::from_millis(200)), 0.25);
        assert_eq!(transition_progress(elapsed, Duration::from_millis(100)), 0.5);
        assert_eq!(transition_progress(elapsed, Duration::from_millis(400)), 0.125);
        assert_eq!(transition_progress(Duration::ZERO, Duration::ZERO), 1.0);
        assert_eq!(
            transition_progress(Duration::from_secs(1), Duration::from_millis(200)),
            1.0
        );
    }
}
