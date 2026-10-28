use super::{
    DirectOutput, DrmNode, Duration, Ferese, Instant, Monotonic, Output, OutputMode, TimeoutAction,
    Timer, arm_animation_timer, crtc, render_output, sleep_locked_outputs,
};
use crate::frame_scheduler::FramePlan;
use smithay::backend::renderer::sync::SyncPoint;
use smithay::reexports::calloop::{Interest, Mode, PostAction, generic::Generic};

pub(super) fn monotonic_now() -> Duration {
    smithay::utils::Clock::<Monotonic>::new().now().into()
}

pub(super) fn cancel_output_timers(
    handle: &smithay::reexports::calloop::LoopHandle<'static, Ferese>,
    output: &mut DirectOutput,
) {
    output.completion.retired();
    for token in [
        output.render_timer.take(),
        output.callback_timer.take(),
        output.completion_timer.take(),
        output.render_fence.take(),
    ]
    .into_iter()
    .flatten()
    {
        handle.remove(token);
    }

    output.scheduler.reset(Duration::from_nanos(
        1_000_000_000_000 / OutputMode::from(output.mode).refresh.max(1) as u64,
    ));
}

pub(super) fn request_frame(state: &mut Ferese, node: DrmNode, crtc: crtc::Handle) {
    if state.session_lock.sleeping {
        sleep_locked_outputs(state);
        return;
    }

    let now = monotonic_now();
    let tearing = state
        .direct_backend
        .as_ref()
        .and_then(|backend| backend.devices.get(&node))
        .and_then(|device| device.outputs.get(&crtc))
        .is_some_and(|output| super::presentation_mode::tearing_candidate(state, output));
    let Some(output) = state
        .direct_backend
        .as_mut()
        .filter(|backend| backend.can_render())
        .and_then(|backend| backend.devices.get_mut(&node))
        .filter(|device| device.drm.is_active())
        .and_then(|device| device.outputs.get_mut(&crtc))
    else {
        return;
    };

    if !output.power.can_render() {
        return;
    }

    if output.scheduler.set_tearing(tearing)
        && let Some(timer) = output.render_timer.take()
    {
        state.loop_handle.remove(timer);
    }

    output
        .render_metrics
        .record_request(output.render_timer.is_some(), output.frame_pending);
    output.scheduler.request(now);
    arm_frame(state, node, crtc);
}

pub(super) fn arm_frame(state: &mut Ferese, node: DrmNode, crtc: crtc::Handle) {
    let now = monotonic_now();
    let Some(output) = state
        .direct_backend
        .as_mut()
        .filter(|backend| backend.can_render())
        .and_then(|backend| backend.devices.get_mut(&node))
        .filter(|device| device.drm.is_active())
        .and_then(|device| device.outputs.get_mut(&crtc))
    else {
        return;
    };

    if output.render_timer.is_some()
        || output.frame_pending
        || !output.power.can_render()
        || state.session_lock.sleeping
    {
        return;
    }

    let Some(plan) = output.scheduler.plan(now) else {
        return;
    };

    let identity = output.output.clone();
    // Even an immediate repaint is queued. Bursts of input and surface commits
    // in the same event-loop dispatch are folded into one scene build.
    let deadline = Instant::now() + plan.render_at.saturating_sub(now);
    match state
        .loop_handle
        .insert_source(Timer::from_deadline(deadline), move |_, _, state| {
            dispatch_frame(state, node, crtc, &identity);
            TimeoutAction::Drop
        }) {
        Ok(token) => {
            if let Some(output) = state
                .direct_backend
                .as_mut()
                .and_then(|backend| backend.devices.get_mut(&node))
                .and_then(|device| device.outputs.get_mut(&crtc))
            {
                output.render_timer = Some(token);
            }
        }

        Err(error) => tracing::warn!(%error, "could not schedule DRM frame"),
    }
}

fn dispatch_frame(state: &mut Ferese, node: DrmNode, crtc: crtc::Handle, identity: &Output) {
    let Some(output) = state
        .direct_backend
        .as_mut()
        .filter(|backend| backend.can_render())
        .and_then(|backend| backend.devices.get_mut(&node))
        .filter(|device| device.drm.is_active())
        .and_then(|device| device.outputs.get_mut(&crtc))
        .filter(|output| &output.output == identity)
    else {
        return;
    };

    output.render_timer = None;
    if output.frame_pending || !output.power.can_render() || state.session_lock.sleeping {
        return;
    }

    let started = monotonic_now();
    let Some(plan) = output.scheduler.begin(started) else {
        return;
    };

    state.advance_animations(Instant::now());

    let output_animating = render_output(state, node, crtc, plan, started);

    if output_animating {
        request_frame(state, node, crtc);
    }

    arm_animation_timer(state);
}

pub(super) fn record_frame_schedule(
    output: &mut DirectOutput,
    plan: FramePlan,
    started: Duration,
    finished: Duration,
    submitted: bool,
) {
    output
        .scheduler
        .rendered(plan, finished, finished.saturating_sub(started), submitted);
    if std::env::var_os("FERESE_TRACE_PERFORMANCE").is_some() {
        tracing::debug!(target: "ferese::render", output = %output.output.name(),
            render_target_us = plan.render_at.as_micros(), presentation_target_us = plan.present_at.as_micros(),
            target_phase_known = plan.phase_known,
            render_started_us = started.as_micros(), timer_lateness_us = started.saturating_sub(plan.render_at).as_micros(),
            render_us = finished.saturating_sub(started).as_micros(), submitted, "DRM frame schedule");
    }
}

pub(super) fn watch_render_ready(
    state: &Ferese,
    node: DrmNode,
    crtc: crtc::Handle,
    output: &mut DirectOutput,
    plan: FramePlan,
    started: Duration,
    sync: SyncPoint,
) {
    if let Some(token) = output.render_fence.take() {
        state.loop_handle.remove(token);
    }
    if sync.is_reached() {
        output
            .scheduler
            .render_ready(plan, monotonic_now().saturating_sub(started));
        return;
    }
    let Some(fd) = sync.export() else {
        return;
    };
    let identity = output.output.clone();
    match state.loop_handle.insert_source(
        Generic::new(fd, Interest::READ, Mode::OneShot),
        move |_, _, state| {
            if let Some(output) = state
                .direct_backend
                .as_mut()
                .and_then(|backend| backend.devices.get_mut(&node))
                .and_then(|device| device.outputs.get_mut(&crtc))
                .filter(|output| output.output == identity)
            {
                output.render_fence = None;
                if sync.is_reached() {
                    let cost = monotonic_now().saturating_sub(started);
                    output.scheduler.render_ready(plan, cost);
                    if std::env::var_os("FERESE_TRACE_PERFORMANCE").is_some() {
                        tracing::debug!(target: "ferese::render", output = %identity.name(),
                            presentation_target_us = plan.present_at.as_micros(),
                            dispatch_to_ready_us = cost.as_micros(), "DRM render fence ready");
                    }
                }
            }
            Ok(PostAction::Remove)
        },
    ) {
        Ok(token) => output.render_fence = Some(token),
        Err(error) => tracing::debug!(%error, "could not observe DRM render readiness"),
    }
}
