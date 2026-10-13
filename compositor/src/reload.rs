//! Filesystem events wake the loop; only pending edits need a debounce timer.
use std::path::PathBuf;

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
        let (_, runtime, candidate) = crate::config::prepare_document(&document, directory)?;
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
                                    crate::config::read_source(path)
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
        self.ipc_events.config_revision = self.ipc_events.config_revision.wrapping_add(1);
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
        self.reload_config_source(crate::config::read_source(&path)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
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
    fn wallpaper_only_reload_applies_path_and_mode_changes() {
        if !crate::startup_tests::private_runtime("reload::tests::wallpaper_only_reload_applies_path_and_mode_changes")
        {
            return;
        }
        let mut events = calloop::EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        let directory = tempfile::tempdir().unwrap();
        let image_path = directory.path().join("wallpaper.png");
        image::RgbaImage::new(2, 2).save(&image_path).unwrap();
        for mode in ["fit", "fill"] {
            let source = format!("wallpaper {{ path \"{}\"; mode \"{mode}\"; }}", image_path.display());
            let (_, runtime, candidate) = crate::config::prepare(&source, directory.path()).unwrap();
            // Model a settled theme; changing only the wallpaper must still apply.
            state.theme_engine.live.theme = candidate.theme.clone();
            state.theme_engine.live.presented = candidate.theme;
            state.reload_config_source(source.clone()).unwrap();
            assert_eq!(state.wallpaper.configuration(), &runtime.wallpaper);
            let revision = state.ipc_events.config_revision;
            state.reload_config_source(source).unwrap();
            assert_eq!(state.ipc_events.config_revision, revision);
        }
        let candidate = crate::config::prepare("", directory.path()).unwrap().2;
        state.theme_engine.live.theme = candidate.theme.clone();
        state.theme_engine.live.presented = candidate.theme.clone();
        let wallpaper = crate::wallpaper::WallpaperConfig {
            path: None,
            ..state.wallpaper.configuration().clone()
        };
        state.accept_theme(candidate, wallpaper.clone());
        assert_eq!(state.wallpaper.configuration(), &wallpaper);
    }

    #[test]
    fn rejected_keymap_reload_preserves_other_configuration() {
        if !crate::startup_tests::private_runtime("reload::tests::rejected_keymap_reload_preserves_other_configuration")
        {
            return;
        }
        let mut events = calloop::EventLoop::try_new().unwrap();
        let mut state = crate::startup_tests::state(&mut events);
        let before_xwayland = state.xwayland_config.enabled;
        let before_activities = state.activities.snapshot().revision;
        let before_layout = state.workspaces.active().layout.mode();
        let before_input = state.input_settings.clone();
        let mut runtime = crate::config::Config::default().runtime_config().unwrap();
        runtime.xwayland.enabled = !before_xwayland;
        runtime.layout_mode = ferese_core::LayoutMode::Tree;
        // Exercise application failure after preparation, for example when an
        // XKB file disappears before the compositor installs the new keymap.
        runtime.input_settings.xkb_layout = "ferese_missing_keymap_for_reload_test".into();
        let error = state.apply_runtime_config(runtime, &serde_json::json!({})).unwrap_err();
        assert!(error.contains("keymap reload failed"), "{error}");
        assert_eq!(state.xwayland_config.enabled, before_xwayland);
        assert_eq!(state.activities.snapshot().revision, before_activities);
        assert_eq!(state.workspaces.active().layout.mode(), before_layout);
        assert_eq!(state.input_settings, before_input);

        let mut runtime = crate::config::Config::default().runtime_config().unwrap();
        runtime.xwayland.enabled = !before_xwayland;
        runtime.layout_mode = ferese_core::LayoutMode::Tree;
        state.apply_runtime_config(runtime, &serde_json::json!({})).unwrap();
        assert_eq!(state.xwayland_config.enabled, !before_xwayland);
        assert_eq!(state.workspaces.active().layout.mode(), ferese_core::LayoutMode::Tree);
        assert!(state.activities.snapshot().revision > before_activities);
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

        let (_, runtime, _) = crate::config::prepare("", std::path::Path::new(".")).unwrap();
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
            let already_applied = state.config_source.as_deref() == Some(valid);
            let before = state.ipc_events.config_revision;
            let theme_before = state.theme_engine.live.theme.clone();
            let (reply, received) = std::sync::mpsc::sync_channel(1);
            state.queue_config_reload(index == 1, Some((42, reply))).unwrap();
            if index == 1 {
                // Present a frame while the duplicate reload is queued. Theme
                // revisions include these frames; config revisions do not.
                std::thread::sleep(Duration::from_millis(20));
                state.poll_theme();
            }
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
            let applied = source == Some(valid) && !already_applied;
            assert_eq!(state.ipc_events.config_revision, before + u64::from(applied));
            if already_applied {
                assert_eq!(state.theme_engine.live.theme, theme_before);
            }
        }
    }

    #[test]
    fn live_reload_keeps_focus_effect_independent_of_theme_transitions() {
        if std::env::var_os("FERESE_DIM_RELOAD_TEST_CHILD").is_none() {
            use std::os::unix::fs::PermissionsExt;

            let directory = Directory::new();
            std::fs::set_permissions(&directory.0, std::fs::Permissions::from_mode(0o700)).unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "reload::tests::live_reload_keeps_focus_effect_independent_of_theme_transitions",
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
        let source = "appearance { focus-effect { inactive-opacity 0.8; inactive-dim 0.25; }; }";
        let directory = Directory::new();
        let (_, runtime, candidate) = crate::config::prepare(source, &directory.0).unwrap();
        let mut event_loop = smithay::reexports::calloop::EventLoop::try_new().unwrap();
        let display = smithay::reexports::wayland_server::Display::new().unwrap();
        let mut state = crate::Ferese::new(&mut event_loop, display, runtime).unwrap();
        state.config_source = Some(source.into());
        state.theme_engine.live.theme = candidate.theme.clone();
        state.theme_engine.live.presented = candidate.theme;

        for inactive_opacity in [1.0, 0.75, 0.9] {
            let source =
                format!("appearance {{ focus-effect {{ inactive-opacity {inactive_opacity}; inactive-dim 0.25; }}; }}");
            state.reload_config_source(source).unwrap();
            assert_eq!(state.focus_effect.inactive_opacity, inactive_opacity);
            let (alpha, dim) = state.focus_effect.resolve(0.0, true, 0.0);
            assert_eq!(alpha, inactive_opacity as f32);
            assert_eq!(dim, 0.25);
        }
        for enabled in [false, true] {
            state.reload_config_source(format!("appearance {{ focus-effect {{ enabled #{enabled}; active-opacity 0.95; inactive-opacity 0.8; inactive-dim 0.25; }}; }}")).unwrap();
            assert_eq!(state.focus_effect.enabled, enabled);
            assert_eq!(state.focus_effect.inactive_opacity, 0.8);
            assert_eq!(state.focus_effect.inactive_dim, 0.25);
            assert_eq!(
                state.focus_effect.resolve(0.0, true, 0.0),
                if enabled { (0.8, 0.25) } else { (1.0, 0.0) }
            );
        }
        let previous = state.focus_effect;
        assert!(
            state
                .reload_config_source("appearance { focus-effect { active-opacity 1.1; }; }".into())
                .is_err()
        );
        assert_eq!(state.focus_effect, previous);
        state
            .reload_config_source("appearance { focus-effect { inactive-dim 0; }; }; theme { mode \"light\"; }".into())
            .unwrap();
        state.poll_theme();
        assert_eq!(state.focus_effect.inactive_dim, 0.0);
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
        let (_, runtime, _) = crate::config::prepare("xwayland { enabled #false; }", &directory.0).unwrap();
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
        let (_, runtime, _) = crate::config::prepare(source, &directory.0).unwrap();
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
    fn source_size_limit_uses_the_production_reader_and_parser() {
        let directory = Directory::new();
        let path = directory.0.join("config.kdl");
        let source = format!("//{}", "x".repeat(60 * 1024 - 2));
        std::fs::write(&path, &source).unwrap();
        assert_eq!(crate::config::read_source(&path).unwrap(), source);
        assert!(Prepared::new(source.clone(), &directory.0).is_ok());
        let oversized = source + "x";
        std::fs::write(&path, &oversized).unwrap();
        assert!(crate::config::read_source(&path).is_err());
        assert!(Prepared::new(oversized, &directory.0).is_err());
    }
}
