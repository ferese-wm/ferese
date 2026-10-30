use std::time::{Duration, Instant};

use super::Discovery;

#[derive(Default)]
pub(super) struct Retry {
    pub requested: Discovery,
    confirmed: Option<Discovery>,
    deadline: Option<Instant>,
    failures: u32,
}

impl Retry {
    pub fn next_retry(&self) -> Option<Instant> {
        if self.confirmed == Some(self.requested) {
            None
        } else {
            self.deadline
        }
    }

    pub fn request(&mut self, kind: Discovery) {
        if self.requested != kind {
            self.failures = 0;
            self.deadline = None;
        }
        self.requested = kind;
    }

    pub fn complete(&mut self, success: bool, now: Instant) {
        if success {
            self.confirmed = Some(self.requested);
            self.deadline = None;
            self.failures = 0;
        } else {
            self.confirmed = None;
            self.deadline = Some(now + Duration::from_secs(1 << self.failures.min(3)));
            self.failures = self.failures.saturating_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_discovery_retries_with_bounded_backoff_until_confirmed_or_closed() {
        let now = Instant::now();
        let mut retry = Retry::default();
        retry.request(Discovery::Bluetooth);
        for seconds in [1, 2, 4, 8, 8] {
            retry.complete(false, now);
            assert_eq!(retry.confirmed, None);
            assert_eq!(retry.deadline, Some(now + Duration::from_secs(seconds)));
        }
        retry.complete(true, now);
        assert_eq!(retry.confirmed, Some(Discovery::Bluetooth));
        assert_eq!(retry.deadline, None);
        retry.complete(false, now);
        retry.request(Discovery::None);
        assert_eq!(retry.deadline, None);
        retry.complete(true, now);
        assert_eq!(retry.confirmed, Some(Discovery::None));
    }
}
