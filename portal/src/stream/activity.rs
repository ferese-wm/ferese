//! Capture failures belong to the consumer activity that submitted their buffer.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Token(u64);

#[derive(Default)]
pub(super) struct Activity {
    active: bool,
    generation: u64,
    retries: u8,
}

const MAX_RETRIES: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FailureKind {
    Unknown,
    Stopped,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FailureAction {
    Discard,
    Retry,
    FallbackToShm,
    Stop,
}

impl Activity {
    pub(super) fn active(&self) -> bool {
        self.active
    }

    /// Return whether a consumer has just resumed. Duplicate notifications keep
    /// their identity so they do not retire a capture belonging to this activity.
    pub(super) fn set_active(&mut self, active: bool) -> bool {
        let resumed = active && !self.active;
        if active != self.active {
            self.generation = self.generation.wrapping_add(1);
            self.active = active;
            self.retries = 0;
        }
        resumed
    }

    pub(super) fn token(&self) -> Token {
        Token(self.generation)
    }

    pub(super) fn ready(&mut self) {
        self.retries = 0;
    }

    pub(super) fn failure_action(
        &mut self,
        submitted: Token,
        source_stopped: bool,
        dma: bool,
        kind: FailureKind,
    ) -> FailureAction {
        if source_stopped || kind == FailureKind::Stopped {
            FailureAction::Stop
        } else if !self.active || self.token() != submitted {
            // A pause can cancel a workspace copy. Its late failure must not
            // stop a resumed consumer or disable an otherwise working GPU path.
            FailureAction::Discard
        } else if dma {
            FailureAction::FallbackToShm
        } else if kind == FailureKind::Unknown && self.retries < MAX_RETRIES {
            // Unknown permits retry. Bound transient capture invalidations so
            // a broken active source cannot keep a worker retrying forever.
            self.retries += 1;
            FailureAction::Retry
        } else {
            FailureAction::Stop
        }
    }
}

#[cfg(test)]
#[path = "activity_tests.rs"]
mod tests;
