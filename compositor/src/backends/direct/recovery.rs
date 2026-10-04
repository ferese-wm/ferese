//! Uncertain KMS completion requires device reconciliation, not buffer reuse.
use super::*;

const COMPLETION_TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(feature = "drm-fault-injection")]
pub(super) fn inject(mode: &str) -> bool {
    use std::sync::LazyLock;
    use std::sync::atomic::{AtomicBool, Ordering};

    static MODE: LazyLock<Option<String>> = LazyLock::new(|| std::env::var("FERESE_TEST_DRM_FAILURE").ok());
    static USED: AtomicBool = AtomicBool::new(false);
    MODE.as_deref() == Some(mode) && !USED.swap(true, Ordering::AcqRel)
}

#[derive(Default)]
pub(super) struct Completion {
    pending: Option<Instant>,
}

impl Completion {
    fn queued(&mut self, now: Instant) {
        self.pending = Some(now);
    }

    pub fn retired(&mut self) {
        self.pending = None;
    }

    fn remaining(&self, now: Instant) -> Option<Duration> {
        self.pending
            .map(|queued| COMPLETION_TIMEOUT.saturating_sub(now.saturating_duration_since(queued)))
    }
}

// Keep the submission result and the recovery decision together. An error must
// not be mistaken for a completed frame with no presentation feedback.
pub(super) fn retirement<T, E>(result: Result<Option<T>, E>) -> (Option<T>, bool, Option<E>) {
    match result {
        Ok(feedback) => {
            let retired = feedback.is_some();
            (feedback, retired, None)
        }
        Err(error) => (None, false, Some(error)),
    }
}

pub(super) fn arm_completion(
    handle: &smithay::reexports::calloop::LoopHandle<'static, Ferese>,
    node: DrmNode,
    crtc: crtc::Handle,
    output: &mut DirectOutput,
) -> bool {
    output.completion.queued(Instant::now());
    if output.completion_timer.is_some() {
        return true;
    }

    let identity = output.output.clone();
    match handle.insert_source(Timer::from_duration(COMPLETION_TIMEOUT), move |_, _, state| {
        let Some(output) = state
            .direct_backend
            .as_mut()
            .and_then(|backend| backend.devices.get_mut(&node))
            .and_then(|device| device.outputs.get_mut(&crtc))
            .filter(|output| output.output == identity)
        else {
            return TimeoutAction::Drop;
        };

        if let Some(remaining) = output.completion.remaining(Instant::now()) {
            if !remaining.is_zero() {
                return TimeoutAction::ToDuration(remaining);
            }

            output.completion_timer = None;
            tracing::error!(?node, ?crtc, "DRM frame completion timed out; reconciling device");
            reconcile_device(state, node);
        } else {
            output.completion_timer = None;
        }

        TimeoutAction::Drop
    }) {
        Ok(token) => {
            output.completion_timer = Some(token);
            true
        }
        Err(error) => {
            tracing::error!(%error, ?node, ?crtc, "could not arm DRM completion watchdog");
            false
        }
    }
}

pub(super) fn reconcile_device(state: &mut Ferese, node: DrmNode) {
    let Some(backend) = state.direct_backend.as_mut() else {
        return;
    };
    backend.topology.dirty = true;
    if !backend.topology.invalidated.insert(node) {
        return;
    }

    // Defer teardown until the DRM notifier callback has returned. Inactive
    // sessions retain the invalidation for fresh enumeration on activation.
    let active = backend.active;
    if active {
        match state.loop_handle.insert_source(Timer::immediate(), |_, _, state| {
            reconcile_outputs(state, false);
            TimeoutAction::Drop
        }) {
            Ok(_) => (),
            Err(error) => tracing::error!(%error, "could not schedule DRM device recovery"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_retirement_does_not_release_pending_buffer_ownership() {
        let now = Instant::now();
        let mut completion = Completion::default();
        completion.queued(now);
        let (feedback, retired, error) = retirement::<(), _>(Err("injected KMS retirement failure"));
        assert_eq!(feedback, None);
        assert!(!retired);
        assert!(error.is_some(), "the production handler must reconcile the device");
        assert_eq!(completion.remaining(now), Some(COMPLETION_TIMEOUT));
        assert_eq!(retirement::<(), &str>(Ok(None)), (None, false, None));
        assert_eq!(retirement::<(), &str>(Ok(Some(()))), (Some(()), true, None));

        // Only successful retirement or device teardown can end uncertainty.
        completion = Completion::default();
        assert_eq!(completion.remaining(now), None);
    }

    #[test]
    fn missing_completion_remains_pending_until_hardware_reconciliation() {
        let now = Instant::now();
        let mut completion = Completion::default();
        completion.queued(now);
        assert_eq!(
            completion.remaining(now + COMPLETION_TIMEOUT - Duration::from_millis(1)),
            Some(Duration::from_millis(1))
        );
        // A timeout observes uncertainty; it never frees the current buffer.
        assert_eq!(completion.remaining(now + COMPLETION_TIMEOUT), Some(Duration::ZERO));
        assert_eq!(
            completion.remaining(now + COMPLETION_TIMEOUT + Duration::from_secs(1)),
            Some(Duration::ZERO)
        );
        completion.retired();
        assert_eq!(completion.remaining(now), None);
        completion.queued(now + Duration::from_secs(3));
        assert_eq!(
            completion.remaining(now + Duration::from_secs(4)),
            Some(Duration::from_secs(1)),
            "existing timer must respect the newer frame's deadline"
        );
    }
}
