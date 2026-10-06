//! Filesystem events wake the loop; only pending edits need a debounce timer.
#[cfg(test)]
use std::io::Read;
use std::path::PathBuf;
#[cfg(test)]
use std::sync::mpsc;
#[cfg(test)]
use std::time::{Duration, Instant};

#[cfg(test)]
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

#[cfg(test)]
fn read_source(path: &std::path::Path) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut source = String::new();
    file.take(60 * 1024 + 1)
        .read_to_string(&mut source)
        .map_err(|e| e.to_string())?;
    if source.len() > 60 * 1024 {
        return Err("configuration exceeds the 60 KiB live-reload limit".into());
    }
    Ok(source)
}

#[cfg(test)]
pub(crate) struct ConfigMonitor {
    _watcher: RecommendedWatcher,
    events: mpsc::Receiver<()>,
    path: PathBuf,
    dirty: Option<Instant>,
    observed: Option<String>,
}

#[cfg(test)]
impl ConfigMonitor {
    #[cfg(test)]
    pub(crate) fn new(path: PathBuf) -> notify::Result<Self> {
        Self::with_wakeup(path, None)
    }

    pub(crate) fn with_wakeup(
        path: PathBuf,
        wakeup: Option<smithay::reexports::calloop::LoopSignal>,
    ) -> notify::Result<Self> {
        let (sender, events) = mpsc::sync_channel(1);
        let target = path.clone();
        let event_wakeup = wakeup.clone();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(event) = event
                && !event.kind.is_access()
                && event.paths.iter().any(|p| p == &target || target.starts_with(p))
            {
                let _ = sender.try_send(());
                if let Some(wakeup) = &event_wakeup {
                    wakeup.wakeup();
                }
            }
        })?;
        // Watch the directory, not the inode: atomic editor saves replace it.
        let mut directory = path.parent().unwrap_or(std::path::Path::new("."));
        while !directory.is_dir() {
            directory = directory.parent().unwrap_or(std::path::Path::new("."));
        }
        watcher.watch(
            directory,
            if Some(directory) == path.parent() {
                RecursiveMode::NonRecursive
            } else {
                RecursiveMode::Recursive
            },
        )?;
        let initial_dirty = path.is_file().then(Instant::now);
        if initial_dirty.is_some()
            && let Some(wakeup) = wakeup
        {
            wakeup.wakeup();
        }
        Ok(Self {
            _watcher: watcher,
            events,
            path,
            dirty: initial_dirty,
            observed: None,
        })
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.dirty.map(|since| since + Duration::from_millis(120))
    }

    pub(crate) fn poll(&mut self, now: Instant) -> Option<Result<String, String>> {
        if self.events.try_iter().next().is_some() {
            self.dirty = Some(now);
        }
        if self
            .dirty
            .is_none_or(|since| now.saturating_duration_since(since) < Duration::from_millis(120))
        {
            return None;
        }
        self.dirty = None;
        let source = match read_source(&self.path) {
            Ok(source) => source,
            Err(error) => {
                return Some(Err(format!(
                    "{}: {error}; retaining active config",
                    self.path.display()
                )));
            }
        };
        if self.observed.as_ref() == Some(&source) {
            return None;
        }
        self.observed = Some(source.clone());
        Some(Ok(source))
    }
}

#[derive(Clone)]
struct Prepared {
    source: String,
    sections: serde_json::Value,
    runtime: crate::RuntimeConfig,
    candidate: ferese_config::theme::Candidate,
}

impl Prepared {
    fn new(source: String, directory: &std::path::Path) -> Result<Self, String> {
        Self::with_watch(source, directory, &mut Vec::new())
    }

    fn with_watch(source: String, directory: &std::path::Path, watched: &mut Vec<PathBuf>) -> Result<Self, String> {
        if source.len() > 60 * 1024 {
            return Err("configuration exceeds the 60 KiB live-reload limit".into());
        }

        let document = ferese_config::Document::parse(&source).map_err(|e| e.to_string())?;
        if let Some(value) = document.get("theme")
            && let Ok(policy) = serde_json::from_value::<ferese_config::theme::Policy>(value.clone())
        {
            watched.extend(
                [&policy.file, &policy.light.file, &policy.dark.file]
                    .into_iter()
                    .flatten()
                    .chain(policy.custom_themes.values().map(|theme| &theme.file))
                    .filter(|path| !path.as_os_str().is_empty())
                    .map(|path| ferese_config::theme::theme_path(directory, path)),
            );
        }
        let (_, runtime, candidate) = crate::theme::prepare_document(&document, directory)?;
        let mut sections = document.value().clone();
        if let Some(object) = sections.as_object_mut() {
            object.remove("theme");
        }

        Ok(Self {
            source,
            sections,
            runtime,
            candidate,
        })
    }
}

fn file_stamps(files: &[PathBuf]) -> Vec<(PathBuf, Option<(std::time::SystemTime, u64)>)> {
    files
        .iter()
        .map(|path| {
            let stamp = path
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok().map(|time| (time, m.len())));
            (path.clone(), stamp)
        })
        .collect()
}

type Reply = (u64, std::sync::mpsc::SyncSender<ferese_ipc::Response>);
struct Request {
    force: bool,
    fallback: Option<String>,
    reply: Option<Reply>,
}

pub(crate) struct Worker {
    requests: std::sync::mpsc::SyncSender<Request>,
}

impl Worker {
    pub(crate) fn new(
        event_loop: &mut calloop::EventLoop<'static, crate::Ferese>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (requests, receiver) = std::sync::mpsc::sync_channel::<Request>(8);
        let (results, events) = calloop::channel::channel::<(Result<Prepared, String>, Vec<PathBuf>, Option<Reply>)>();
        event_loop.handle().insert_source(events, |event, _, state| {
            if let calloop::channel::Event::Msg((result, watched, reply)) = event {
                state.theme_engine.watch_files(&watched);
                let result = result.and_then(|prepared| state.apply_prepared_config(prepared));
                if let Err(error) = &result {
                    state.theme_engine.reject(error.clone());
                }

                if let Some((id, reply)) = reply {
                    let response = match result {
                        Ok(()) => ferese_ipc::Response::success(id, serde_json::json!({})),
                        Err(error) => ferese_ipc::Response::error(id, "invalid_config", error),
                    };
                    let _ = reply.try_send(response);
                }
            }
        })?;
        std::thread::Builder::new()
            .name("ferese-config".into())
            .spawn(move || {
                let mut cached: Option<Prepared> = None;
                let mut stamps = Vec::new();
                for request in receiver {
                    let started = std::time::Instant::now();
                    let path = crate::config::config_path();
                    let directory = path
                        .as_ref()
                        .and_then(|p| p.parent())
                        .unwrap_or(std::path::Path::new("."));
                    let mut watched = Vec::new();
                    let result = path
                        .as_ref()
                        .map_or_else(
                            || request.fallback.ok_or_else(|| "No configuration available".into()),
                            |path| {
                                if path.exists() || !request.force {
                                    crate::theme::read_source(path)
                                } else {
                                    Ok(String::new())
                                }
                            },
                        )
                        .and_then(|source| {
                            // Watcher and explicit reloads share the same cache.
                            // Auto mode still resolves time and system appearance.
                            if let Some(previous) = &cached
                                && previous.source == source
                                && previous.candidate.policy.mode != ferese_config::theme::Mode::Auto
                                && file_stamps(&previous.candidate.files) == stamps
                            {
                                return Ok(previous.clone());
                            }

                            let prepared = Prepared::with_watch(source, directory, &mut watched)?;
                            stamps = file_stamps(&prepared.candidate.files);
                            cached = Some(prepared.clone());
                            Ok(prepared)
                        });
                    tracing::debug!(
                        elapsed_us = started.elapsed().as_micros() as u64,
                        "configuration prepared on worker"
                    );
                    if results.send((result, watched, request.reply)).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self { requests })
    }
}

impl crate::Ferese {
    pub(crate) fn queue_config_reload(&self, force: bool, reply: Option<Reply>) -> Result<(), String> {
        let request = Request {
            force,
            fallback: self.config_source.clone(),
            reply,
        };
        let worker = self.config_worker.as_ref().ok_or("configuration worker unavailable")?;
        if let Err(error) = worker.requests.try_send(request) {
            let (std::sync::mpsc::TrySendError::Full(request) | std::sync::mpsc::TrySendError::Disconnected(request)) =
                error;
            if let Some((id, reply)) = request.reply {
                let _ = reply.try_send(ferese_ipc::Response::error(
                    id,
                    "reload_busy",
                    "Configuration reload queue is full",
                ));
            }
            return Err("Configuration reload queue is full".into());
        }

        Ok(())
    }

    fn apply_prepared_config(&mut self, prepared: Prepared) -> Result<(), String> {
        let Prepared {
            source,
            sections,
            mut runtime,
            candidate,
        } = prepared;
        // Identical watcher/IPC reloads do not restart a theme transition.
        if self.config_source.as_ref() == Some(&source)
            && candidate.theme == self.theme_engine.live.theme
            && candidate.families == self.theme_engine.live.families
            && candidate.policy.mode == self.theme_engine.live.mode
            && candidate.warnings == self.theme_engine.live.warnings
            && candidate.fallback_note == self.theme_engine.live.fallback_note
            && self.theme_engine.matches_files(&candidate.files)
            && self.theme_engine.live.error.is_none()
        {
            if let Err(error) = self.theme_engine.arm_clock(candidate.next_transition) {
                tracing::warn!(%error, "cannot arm appearance schedule");
            }
            return Ok(());
        }

        let wallpaper = runtime.wallpaper.clone();
        runtime.wallpaper = self.wallpaper.configuration().clone();
        let previous_theme = self.theme_settings;
        if self.config_sections.is_none() {
            self.config_sections = self
                .config_source
                .as_deref()
                .and_then(|s| ferese_config::Document::parse(s).ok())
                .map(|document| {
                    let mut value = document.value().clone();
                    if let Some(object) = value.as_object_mut() {
                        object.remove("theme");
                    }
                    value
                });
        }
        if self.config_sections.as_ref() != Some(&sections) {
            self.apply_runtime_config(runtime, &sections)?;
        }

        self.overview
            .set_font_family(candidate.theme.tokens.typography.font_family.clone());
        self.theme_settings = previous_theme;
        self.accept_theme(candidate, wallpaper);
        self.config_sections = Some(sections);
        self.config_source = Some(source.clone());
        self.shell_resources.retain(|resource| {
            if let Ok(shell) = resource.upgrade() {
                if smithay::reexports::wayland_server::Resource::version(&shell) >= 2 {
                    crate::shell_control::send_shell_config(&shell, &source);
                }
                true
            } else {
                false
            }
        });
        tracing::info!("configuration reloaded live");
        Ok(())
    }

    pub(crate) fn reload_config_source(&mut self, source: String) -> Result<(), String> {
        let directory = crate::config::config_path()
            .and_then(|p| p.parent().map(ToOwned::to_owned))
            .unwrap_or_else(|| PathBuf::from("."));
        let prepared = Prepared::new(source, &directory)?;
        self.apply_prepared_config(prepared)
    }

    pub(crate) fn reload_config(&mut self) -> Result<(), String> {
        let path = crate::config::config_path().ok_or("config directory is unavailable")?;
        self.reload_config_source(crate::theme::read_source(&path)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "ferese-reload-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn worker_reload_applies_once_and_rejects_invalid_configuration() {
        const CHILD: &str = "FERESE_WORKER_RELOAD_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            use std::os::unix::fs::PermissionsExt;
            let directory = Directory::new();
            std::fs::set_permissions(&directory.0, std::fs::Permissions::from_mode(0o700)).unwrap();
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "reload::tests::worker_reload_applies_once_and_rejects_invalid_configuration",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("XDG_RUNTIME_DIR", &directory.0)
                .env("XDG_CONFIG_HOME", &directory.0)
                .env_remove("FERESE_SOCKET")
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }

        let (_, runtime, _) = crate::theme::prepare("", std::path::Path::new(".")).unwrap();
        let mut event_loop = calloop::EventLoop::try_new().unwrap();
        let display = smithay::reexports::wayland_server::Display::new().unwrap();
        let mut state = crate::Ferese::new(&mut event_loop, display, runtime).unwrap();
        let path = crate::config::config_path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let valid = "animations { speed 0.5; }";
        std::fs::write(&path, valid).unwrap();
        for (index, source) in [Some(valid), Some(valid), Some("animations { speed -1.0; }"), None]
            .into_iter()
            .enumerate()
        {
            if let Some(source) = source {
                std::fs::write(&path, source).unwrap();
            } else {
                std::fs::remove_file(&path).unwrap();
            }
            let before = state.theme_engine.revision;
            let (reply, received) = std::sync::mpsc::sync_channel(1);
            state.queue_config_reload(index == 1, Some((42, reply))).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            let response = loop {
                event_loop.dispatch(Duration::from_millis(10), &mut state).unwrap();
                if let Ok(response) = received.try_recv() {
                    break response;
                }
                assert!(Instant::now() < deadline, "worker did not reply");
            };
            assert_eq!(response.id, 42);
            assert_eq!(response.error.is_some(), source != Some(valid));
            assert_eq!(
                state.animation_duration(Duration::from_millis(100)),
                Duration::from_millis(200)
            );
            assert_eq!(state.config_source.as_deref(), Some(valid));
            if source == Some(valid) && before > 0 {
                assert_eq!(state.theme_engine.revision, before);
            }
        }
    }

    #[test]
    fn live_reload_keeps_dimming_policy_independent_of_theme_transitions() {
        if std::env::var_os("FERESE_DIM_RELOAD_TEST_CHILD").is_none() {
            use std::os::unix::fs::PermissionsExt;

            let directory = Directory::new();
            std::fs::set_permissions(&directory.0, std::fs::Permissions::from_mode(0o700)).unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "reload::tests::live_reload_keeps_dimming_policy_independent_of_theme_transitions",
                    "--nocapture",
                ])
                .env("FERESE_DIM_RELOAD_TEST_CHILD", "1")
                .env("XDG_RUNTIME_DIR", &directory.0)
                .env("XDG_CONFIG_HOME", &directory.0)
                .env_remove("FERESE_SOCKET")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let source = "appearance { inactive-dim { enabled #true; amount 0.25; }; }";
        let directory = Directory::new();
        let (_, runtime, candidate) = crate::theme::prepare(source, &directory.0).unwrap();
        let mut event_loop = smithay::reexports::calloop::EventLoop::try_new().unwrap();
        let display = smithay::reexports::wayland_server::Display::new().unwrap();
        let mut state = crate::Ferese::new(&mut event_loop, display, runtime).unwrap();
        state.config_source = Some(source.into());
        state.theme_engine.live.theme = candidate.theme.clone();
        state.theme_engine.live.presented = candidate.theme;

        for enabled in [false, true, false] {
            let source = format!("appearance {{ inactive-dim {{ enabled #{enabled}; amount 0.25; }}; }}");
            state.reload_config_source(source).unwrap();
            assert_eq!(state.inactive_dim.enabled, enabled);
            let opacity = crate::dimming::target(
                state.inactive_dim,
                Some(ferese_layout::WindowId(1)),
                ferese_layout::WindowId(2),
                false,
            );
            assert_eq!(opacity, if enabled { 0.25 } else { 0.0 });
        }
        state
            .reload_config_source("appearance { inactive-dim { enabled #false; }; }; theme { mode \"light\"; }".into())
            .unwrap();
        state.poll_theme();
        assert!(!state.inactive_dim.enabled);
    }

    /// The status endpoint must be able to tell "the live service is running
    /// with the settings it started with" from "the file was edited", so the
    /// desired X11 configuration has to follow live reloads.
    #[test]
    fn xwayland_configuration_follows_live_reloads() {
        const CHILD: &str = "FERESE_XWAYLAND_RELOAD_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            use std::os::unix::fs::PermissionsExt;
            let directory = Directory::new();
            std::fs::set_permissions(&directory.0, std::fs::Permissions::from_mode(0o700)).unwrap();
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "reload::tests::xwayland_configuration_follows_live_reloads",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("XDG_RUNTIME_DIR", &directory.0)
                .env("XDG_CONFIG_HOME", &directory.0)
                .env_remove("FERESE_SOCKET")
                .output()
                .unwrap();
            assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
            return;
        }

        let directory = Directory::new();
        let (_, runtime, _) = crate::theme::prepare("xwayland { enabled #false; }", &directory.0).unwrap();
        let mut event_loop = smithay::reexports::calloop::EventLoop::try_new().unwrap();
        let display = smithay::reexports::wayland_server::Display::new().unwrap();
        let mut state = crate::Ferese::new(&mut event_loop, display, runtime).unwrap();

        assert!(
            !state.xwayland_config.enabled,
            "the session starts with the configured value"
        );

        state
            .reload_config_source("xwayland { enabled #true; startup \"eager\"; }".into())
            .unwrap();

        assert!(
            state.xwayland_config.enabled,
            "a reload must update the desired configuration, or restart_required can never become true"
        );
        assert_eq!(
            state.xwayland_config.startup,
            crate::config::XwaylandStartup::Eager,
            "every field follows the reload, not just the enabled flag"
        );
    }

    #[test]
    fn invalid_monitor_reload_keeps_last_good_runtime_configuration() {
        const CHILD: &str = "FERESE_OUTPUT_RELOAD_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            use std::os::unix::fs::PermissionsExt;
            let directory = Directory::new();
            std::fs::set_permissions(&directory.0, std::fs::Permissions::from_mode(0o700)).unwrap();
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "reload::tests::invalid_monitor_reload_keeps_last_good_runtime_configuration",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("XDG_RUNTIME_DIR", &directory.0)
                .env("XDG_CONFIG_HOME", &directory.0)
                .env_remove("FERESE_SOCKET")
                .output()
                .unwrap();
            assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
            return;
        }
        let directory = Directory::new();
        let source = "output-profile mobile { output eDP-1 scale=1.75; }";
        let (_, runtime, _) = crate::theme::prepare(source, &directory.0).unwrap();
        let mut event_loop = smithay::reexports::calloop::EventLoop::try_new().unwrap();
        let display = smithay::reexports::wayland_server::Display::new().unwrap();
        let mut state = crate::Ferese::new(&mut event_loop, display, runtime).unwrap();
        state.config_source = Some(source.into());
        let accepted = state.output_profiles.clone();
        for invalid in [
            "output-profile bad { output eDP-1 scale=0; }",
            "output-profile bad layout=\"mirror\" { output eDP-1; }",
            "output-profile mobile { output eDP-1; }\noutput-profile mobile { output DP-1; }",
        ] {
            assert!(state.reload_config_source(invalid.into()).is_err());
            assert_eq!(state.output_profiles, accepted);
            assert_eq!(state.config_source.as_deref(), Some(source));
        }
    }

    #[test]
    fn runtime_validation_rejects_bad_edits_before_application() {
        for source in [
            "animations {\n    speed 0\n}\n",
            r##"theme {
    focus-ring {
        gradient {
            from "bad"
            to "#ffffff"
        }
    }
}
"##,
            "status {\n    low-battery-threshold \"bad\"\n}\n",
            "layout {\n    inner-gap -1\n}\n",
        ] {
            assert!(
                crate::config::Config::parse_source(source)
                    .and_then(|config| config.runtime_config())
                    .is_err()
            );
        }
        assert!(
            crate::config::Config::parse_source("animations {\n    speed 0.75\n}\n")
                .unwrap()
                .runtime_config()
                .is_ok()
        );
    }

    #[test]
    fn directory_watch_detects_atomic_save_and_does_not_read_during_idle() {
        let directory = Directory::new();
        let path = directory.0.join("config.kdl");
        std::fs::write(&path, "animations {\n    speed 1\n}\n").unwrap();
        let mut monitor = ConfigMonitor::new(path.clone()).unwrap();
        monitor.dirty = Some(Instant::now() - Duration::from_secs(1));
        assert!(monitor.poll(Instant::now()).unwrap().is_ok());
        // With no event, even a deliberately missing path is never read.
        let original_path = monitor.path.clone();
        monitor.path = directory.0.join("missing");
        assert!(monitor.poll(Instant::now()).is_none());
        monitor.path = original_path;
        let temporary = directory.0.join("config.kdl.new");
        std::fs::write(&temporary, "animations {\n    speed 0.75\n}\n").unwrap();
        std::fs::rename(&temporary, &path).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(result) = monitor.poll(Instant::now()) {
                assert_eq!(result.unwrap(), "animations {\n    speed 0.75\n}\n");
                break;
            }
            assert!(Instant::now() < deadline, "atomic save notification never settled");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn source_size_limit_is_bounded_and_debounce_coalesces_edits() {
        let directory = Directory::new();
        let path = directory.0.join("config.kdl");
        std::fs::write(&path, "x".repeat(60 * 1024 + 1)).unwrap();
        assert!(read_source(&path).is_err());
        std::fs::write(&path, "animations {\n    speed 1\n}\n").unwrap();
        let mut monitor = ConfigMonitor::new(path).unwrap();
        let now = Instant::now();
        monitor.dirty = Some(now);
        assert_eq!(monitor.next_deadline(), Some(now + Duration::from_millis(120)));
        assert!(monitor.poll(now + Duration::from_millis(119)).is_none());
        assert!(monitor.poll(now + Duration::from_millis(121)).unwrap().is_ok());
        assert_eq!(monitor.next_deadline(), None);
    }
}
