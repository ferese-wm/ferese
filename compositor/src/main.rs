mod activities;
mod activities_process;
mod backends;
mod buffer_sync;
mod config;
mod cursor;
mod daemon;
mod desktop_protocols;
mod display_presentation;
mod dmabuf_feedback;
mod dmabuf_imports;
mod dnd;
mod effects;
mod floating;
mod frame_scheduler;
mod gamma;
mod gestures;
mod grabs;
mod handlers;
mod idle_inhibition;
mod input;
mod input_capture;
mod ipc;
mod ipc_events;
mod metrics;
mod monitor_identity;
mod output_management;
mod output_policy;
mod output_transaction;
mod overview;
mod portal_session;
mod portal_shortcuts;
mod presentation;
mod presentation_constraints;
mod presentation_policy;
mod priority;
mod private_client;
mod process;
mod reload;
mod render;
#[cfg(feature = "resize-metrics")]
mod resize_metrics;
mod resize_transaction;
mod resume;
mod session_environment;
mod session_lock;
mod shell_control;
mod stacking;
#[cfg(test)]
mod startup_tests;
mod state;
mod tearing_control;
mod theme;
mod wallpaper;
mod window_effects;
mod window_rules;
mod winit;
mod xwayland;

pub(crate) use session_environment::{SessionEnvironment, SessionPolicy, X11Environment};
pub use state::{Ferese, RuntimeConfig};

use std::error::Error;
use std::io;

use calloop::signals::{Signal, Signals};
use process::{spawn_client, terminate_child, watch_client_exit};
use smithay::reexports::calloop::EventLoop;
use smithay::reexports::wayland_server::Display;
use tracing::{info, warn};

use crate::backends::LaunchConfig;

fn main() -> Result<(), Box<dyn Error>> {
    init_logging();

    // Settings validates a candidate before atomically replacing the user's file.
    // This mode never opens a display, input device, or compositor socket.
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "--check-config") {
        if args.len() != 2 {
            return Err("usage: ferese --check-config PATH".into());
        }

        check_config(std::path::Path::new(&args[1]))?;
        println!("Configuration is valid");
        return Ok(());
    }

    let launch = LaunchConfig::from_environment()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    if launch.backend == backends::BackendKind::Drm {
        priority::init();
    }
    let config_path = config::config_path();
    let startup = config::startup(config_path.as_deref())?;
    if let Some(error) = &startup.error {
        warn!(%error, "configuration could not be loaded at startup; using defaults");
    }
    let mut event_loop = EventLoop::try_new()?;
    let signals = Signals::new(&[Signal::SIGINT, Signal::SIGTERM])?;
    event_loop
        .handle()
        .insert_source(signals, |event, _, state: &mut Ferese| {
            info!(signal = ?event.signal(), "stopping Ferese");
            state.loop_signal.stop();
        })?;
    let display = Display::new()?;
    let endpoint = match launch.backend {
        backends::BackendKind::Drm => ipc::Endpoint::desktop()?,
        backends::BackendKind::Nested => ipc::Endpoint::instance()?,
    };
    let mut state = Ferese::try_new_with_session(
        &mut event_loop,
        display,
        startup.runtime,
        endpoint,
        launch.session_policy,
    )?;
    state.config_source = startup.source;
    state.config_sections = state
        .config_source
        .as_deref()
        .and_then(|source| ferese_config::Document::parse(source).ok())
        .map(|document| {
            let mut value = document.value().clone();
            if let Some(object) = value.as_object_mut() {
                object.remove("theme");
            }
            value
        });
    if let Some(error) = startup.error {
        state
            .theme_engine
            .startup_failed(config_path.as_deref(), error);
    }
    theme::init(&mut event_loop, &mut state, startup.theme)?;
    let _resume_monitor = resume::init(&mut event_loop)?;
    idle_inhibition::media::init(&mut event_loop)?;
    overview::init_font_loader(&mut event_loop, &mut state)?;
    backends::init(launch.backend, &mut event_loop, &mut state)?;

    xwayland::initialize(&mut state);

    info!(socket = ?state.socket_name, backend = ?launch.backend, "Ferese is accepting Wayland clients");

    let (supervisor, child) = if launch.supervise_client {
        (
            Some(process::Supervisor::start(
                &mut state,
                launch.client,
                launch.client_capabilities,
            )),
            None,
        )
    } else {
        (
            None,
            spawn_client(&mut state, launch.client, launch.client_capabilities)
                .map(|child| watch_client_exit(&state.loop_handle, child)),
        )
    };
    let runner = daemon::Runner::start(&mut state);
    state.daemons = Some(runner.clone());
    let result = event_loop.run(None, &mut state, after_dispatch);

    if let Some(backend) = state.direct_backend.as_ref() {
        backend.dump_scheduling_metrics();
    }

    #[cfg(feature = "resize-metrics")]
    state.resize_metrics.dump();

    runner.borrow_mut().stop();
    if let Some(supervisor) = supervisor {
        supervisor.borrow_mut().stop(&state.loop_handle);
    }
    if let Some(child) = child
        && let Some(mut child) = child.borrow_mut().take()
    {
        terminate_child(&mut child);
    }

    xwayland::shutdown_after_loop(&mut state);

    result.map_err(Into::into)
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("ferese=info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

fn check_config(path: &std::path::Path) -> Result<(), String> {
    let source = config::read_source(path)?;
    config::prepare(&source, path.parent().unwrap_or(std::path::Path::new(".")))?;
    Ok(())
}

#[derive(Debug)]
enum RedrawRequest<'outputs> {
    Global,
    Cursor,
    Outputs(&'outputs [smithay::output::Output]),
}

fn after_dispatch(state: &mut Ferese) {
    after_dispatch_with_redraw(state, |state, request| match request {
        RedrawRequest::Global => backends::direct::render_all(state),
        RedrawRequest::Cursor => backends::direct::render_cursor(state),
        RedrawRequest::Outputs(outputs) => backends::direct::render_on(state, outputs),
    });
}

fn after_dispatch_with_redraw(
    state: &mut Ferese,
    mut redraw: impl FnMut(&mut Ferese, RedrawRequest<'_>),
) {
    state.poll_buffer_sync();
    state.poll_presentation_constraints();
    crate::tearing_control::sync(state);
    crate::backends::direct::leasing::poll_lock(state);
    crate::backends::direct::gamma::sync_night_light(state);
    state.poll_window_interaction();
    state.poll_drag();
    state.process_pending_dmabuf_imports();
    state.prune_screencopies();
    state.refresh_capture_sessions();
    state.update_named_cursor(std::time::Instant::now());
    state.poll_theme();
    if state.session_lock.active() || state.input_capture.active() {
        state.set_binding_mode("default");
    }
    // The decoder wakes the loop after publishing its result, including
    // when no output has a pending frame.
    let wallpaper_changed = state.wallpaper.poll();
    // All input/Wayland callbacks have returned, releasing seat locks.
    if state.focus_cycle.is_some()
        && (state.session_lock.active() || state.input_capture.captures(1))
    {
        state.cancel_focus_cycle();
    }

    if state.input_capture.restore_focus {
        state.restore_input_capture_focus();
    }

    state.apply_unlocked_pointer_hint();
    let wallpaper_retry = state.wallpaper.take_retry_wakeup();
    let cursor_changed = std::mem::take(&mut state.cursor_redraw_pending);
    let output_redraws = std::mem::take(&mut state.output_redraw_pending);
    if wallpaper_changed || wallpaper_retry {
        redraw(state, RedrawRequest::Global);
    } else {
        if cursor_changed {
            redraw(state, RedrawRequest::Cursor);
        }

        if !output_redraws.is_empty() {
            redraw(state, RedrawRequest::Outputs(&output_redraws));
        }
    }

    if let Err(error) = state
        .wallpaper
        .arm_retry_timer(&state.loop_handle, std::time::Instant::now())
    {
        warn!(%error, "could not schedule wallpaper upload retry");
    }

    crate::backends::direct::feedback::refresh(state);
    crate::output_management::sync(state);
    crate::desktop_protocols::sync(state);
    state.publish_ipc_events();

    // Clients need registry/sync/configure replies even when no frame is drawn.
    if let Err(error) = state.display_handle.flush_clients() {
        warn!(%error, "failed to flush Wayland clients");
    }
}
