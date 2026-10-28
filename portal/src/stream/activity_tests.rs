use super::{Activity, FailureAction, FailureKind, MAX_RETRIES};

#[test]
fn paused_copy_failure_does_not_stop_shm_or_disable_dma() {
    for dma in [false, true] {
        let mut activity = Activity::default();
        assert!(activity.set_active(true));
        let submitted = activity.token();

        assert!(!activity.set_active(false));
        assert!(!activity.active());
        assert_eq!(
            activity.failure_action(submitted, false, dma, FailureKind::Unknown),
            FailureAction::Discard,
            "canceling a copy while paused must keep its stream reusable"
        );
        assert!(activity.set_active(true));
        assert!(activity.active());
    }
}

#[test]
fn late_failure_from_before_pause_cannot_kill_a_resumed_consumer() {
    for dma in [false, true] {
        let mut activity = Activity::default();
        activity.set_active(true);
        let old = activity.token();
        activity.set_active(false);
        activity.set_active(true);
        let resumed = activity.token();
        assert_ne!(old, resumed);

        assert_eq!(
            activity.failure_action(old, false, dma, FailureKind::Unknown),
            FailureAction::Discard,
            "the old copy may report cancellation after Streaming resumes"
        );
        assert_eq!(
            activity.failure_action(resumed, false, dma, FailureKind::Unknown),
            FailureAction::Retry,
            "a genuine failure of the resumed capture must retain handling"
        );
    }
}

#[test]
fn duplicate_state_notifications_keep_current_failure_ownership() {
    let mut activity = Activity::default();
    let initial = activity.token();
    assert!(!activity.set_active(false));
    assert_eq!(activity.token(), initial);
    assert!(activity.set_active(true));
    let submitted = activity.token();
    assert!(!activity.set_active(true));
    assert_eq!(activity.token(), submitted);
    assert_eq!(
        activity.failure_action(submitted, false, true, FailureKind::Unknown),
        FailureAction::Retry
    );
    assert_eq!(
        activity.failure_action(submitted, false, false, FailureKind::Unknown),
        FailureAction::Retry
    );
}

#[test]
fn a_stopped_source_remains_terminal_during_pause_and_after_resume() {
    for dma in [false, true] {
        let mut activity = Activity::default();
        activity.set_active(true);
        let submitted = activity.token();
        assert_eq!(
            activity.failure_action(submitted, true, dma, FailureKind::Unknown),
            FailureAction::Stop
        );
        activity.set_active(false);
        assert_eq!(
            activity.failure_action(submitted, true, dma, FailureKind::Unknown),
            FailureAction::Stop,
            "source loss must not be hidden by an inactive consumer"
        );
        activity.set_active(true);
        assert_eq!(
            activity.failure_action(submitted, true, dma, FailureKind::Unknown),
            FailureAction::Stop,
            "source loss must not be mistaken for an old pause cancellation"
        );
    }
}

#[test]
fn current_shm_unknown_errors_retry_with_a_limit_and_success_resets_the_streak() {
    let mut activity = Activity::default();
    activity.set_active(true);
    let submitted = activity.token();
    for _ in 0..MAX_RETRIES {
        assert_eq!(
            activity.failure_action(submitted, false, false, FailureKind::Unknown),
            FailureAction::Retry
        );
    }
    assert_eq!(
        activity.failure_action(submitted, false, false, FailureKind::Unknown),
        FailureAction::Stop,
        "a broken current capture must not retry forever"
    );
    activity.ready();
    assert_eq!(
        activity.failure_action(submitted, false, false, FailureKind::Unknown),
        FailureAction::Retry,
        "a completed image ends the consecutive failure streak"
    );
}

#[test]
fn canceled_old_copies_do_not_charge_the_resumed_retry_budget() {
    let mut activity = Activity::default();
    activity.set_active(true);
    let old = activity.token();
    // The previous consumer has exhausted its retries when it disconnects.
    for _ in 0..MAX_RETRIES {
        activity.failure_action(old, false, false, FailureKind::Unknown);
    }
    activity.set_active(false);
    activity.set_active(true);
    let resumed = activity.token();
    for _ in 0..20 {
        assert_eq!(
            activity.failure_action(old, false, false, FailureKind::Unknown),
            FailureAction::Discard
        );
    }
    for _ in 0..MAX_RETRIES {
        assert_eq!(
            activity.failure_action(resumed, false, false, FailureKind::Unknown),
            FailureAction::Retry,
            "only failures submitted by the resumed consumer consume its budget"
        );
    }
    assert_eq!(
        activity.failure_action(resumed, false, false, FailureKind::Unknown),
        FailureAction::Stop
    );
}

#[test]
fn explicit_stopped_and_unrecognized_failures_keep_terminal_handling() {
    for dma in [false, true] {
        let mut activity = Activity::default();
        activity.set_active(true);
        let submitted = activity.token();
        assert_eq!(
            activity.failure_action(submitted, false, dma, FailureKind::Stopped),
            FailureAction::Stop,
            "a stopped event is terminal even before a session stopped event"
        );
        assert_eq!(
            activity.failure_action(submitted, false, dma, FailureKind::Other),
            if dma {
                FailureAction::FallbackToShm
            } else {
                FailureAction::Stop
            }
        );
        activity.set_active(false);
        assert_eq!(
            activity.failure_action(submitted, false, dma, FailureKind::Stopped),
            FailureAction::Stop,
            "an inactive consumer must still observe real source shutdown"
        );
    }
}

#[test]
fn transient_gpu_invalidation_preserves_dma_and_success_resets_retry_budget() {
    let mut activity = Activity::default();
    activity.set_active(true);
    let submitted = activity.token();
    for _ in 0..20 {
        assert_eq!(
            activity.failure_action(submitted, false, true, FailureKind::Unknown),
            FailureAction::Retry,
            "a single invalidated frame must not disable the GPU transport"
        );
        activity.ready();
    }
    for _ in 0..MAX_RETRIES {
        assert_eq!(
            activity.failure_action(submitted, false, true, FailureKind::Unknown),
            FailureAction::Retry
        );
    }
    assert_eq!(
        activity.failure_action(submitted, false, true, FailureKind::Unknown),
        FailureAction::FallbackToShm,
        "a repeatedly failing GPU transfer must eventually fall back"
    );
    for _ in 0..MAX_RETRIES {
        assert_eq!(
            activity.failure_action(submitted, false, false, FailureKind::Unknown),
            FailureAction::Retry,
            "SHM recovery must not inherit an exhausted GPU retry budget"
        );
    }
    assert_eq!(
        activity.failure_action(submitted, false, false, FailureKind::Unknown),
        FailureAction::Stop
    );
}
