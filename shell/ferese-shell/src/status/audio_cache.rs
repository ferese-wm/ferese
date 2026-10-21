//! Query WirePlumber only after audio changes, retaining its route and volume semantics.
mod signals;

use std::sync::Arc;
use std::time::{Duration, Instant};

use signals::Signals;

use super::{Audio, PollState};

pub(super) struct Cache {
    signals: Signals,
    revision: u64,
    action: u64,
    value: Option<Audio>,
    retry_at: Instant,
}

impl Cache {
    pub(super) fn new(wake: Arc<PollState>) -> Self {
        Self {
            signals: Signals::start(wake),
            revision: 0,
            action: u64::MAX,
            value: None,
            retry_at: Instant::now(),
        }
    }

    pub(super) fn read(&mut self, action: u64) -> Option<Audio> {
        self.read_at(action, Instant::now(), super::audio)
    }

    fn read_at(
        &mut self,
        action: u64,
        now: Instant,
        read: impl FnOnce() -> Option<Audio>,
    ) -> Option<Audio> {
        let revision = self.signals.revision();
        // Without a live subscription, keep the old bounded polling fallback.
        // A missing default sink retries slowly, even if the server is connected.
        if !self.signals.alive()
            || revision != self.revision
            || action != self.action
            || (self.value.is_none() && now >= self.retry_at)
        {
            self.value = read();
            self.revision = revision;
            self.action = action;
            self.retry_at = now + Duration::from_secs(5);
        }

        self.value.clone()
    }

    pub(super) fn next_retry(&self) -> Option<Instant> {
        (!self.signals.alive() || self.value.is_none()).then_some(self.retry_at)
    }

    pub(super) fn changed(&self) -> bool {
        self.signals.revision() != self.revision
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Condvar, Mutex};

    use super::*;

    fn cache() -> Cache {
        Cache {
            signals: Signals::test(Arc::new((
                Mutex::new((0, false, None, false, 0)),
                Condvar::new(),
            ))),
            revision: 0,
            action: u64::MAX,
            value: None,
            retry_at: Instant::now(),
        }
    }

    fn sample() -> Option<Audio> {
        Some(Audio {
            volume: 48,
            muted: false,
            output: "Speakers".into(),
        })
    }

    #[test]
    fn unchanged_audio_never_starts_another_command() {
        let mut cache = cache();
        let now = Instant::now();
        assert_eq!(cache.read_at(0, now, sample).unwrap().volume, 48);

        for second in 1..=120 {
            let audio = cache.read_at(0, now + Duration::from_secs(second), || {
                panic!("idle query")
            });
            assert_eq!(audio.unwrap().volume, 48);
        }
    }

    #[test]
    fn coalesced_events_refresh_volume_mute_and_output_once() {
        let mut cache = cache();
        let now = Instant::now();
        cache.read_at(0, now, sample);
        cache.signals.changed();
        cache.signals.changed();
        assert!(cache.changed());

        let audio = cache
            .read_at(0, now, || {
                Some(Audio {
                    volume: 17,
                    muted: true,
                    output: "Headphones".into(),
                })
            })
            .unwrap();
        assert_eq!(audio.volume, 17);
        assert!(audio.muted);
        assert_eq!(audio.output, "Headphones");
        assert!(!cache.changed());
        cache.read_at(0, now, || panic!("duplicate query"));
    }

    #[test]
    fn event_during_query_is_not_lost_before_waiting() {
        let mut cache = cache();
        let state = cache.signals.state.clone();
        cache.read_at(0, Instant::now(), || {
            state.changed();
            sample()
        });
        assert!(cache.changed());
    }

    #[test]
    fn writes_force_a_fresh_read_even_before_the_signal_arrives() {
        let mut cache = cache();
        let now = Instant::now();
        cache.read_at(0, now, sample);
        assert_eq!(
            cache
                .read_at(1, now, || Some(Audio {
                    volume: 30,
                    muted: false,
                    output: "Speakers".into(),
                }))
                .unwrap()
                .volume,
            30
        );
    }

    #[test]
    fn disconnect_removes_stale_audio_and_reconnect_refreshes_it() {
        let mut cache = cache();
        let now = Instant::now();
        cache.read_at(0, now, sample);
        cache.signals.state.set_alive(false);
        assert!(cache.read_at(0, now, || None).is_none());
        cache.signals.state.set_alive(true);
        assert_eq!(cache.read_at(0, now, sample).unwrap().volume, 48);
    }

    #[test]
    fn missing_default_sink_retries_after_five_seconds() {
        let mut cache = cache();
        let now = Instant::now();
        cache.read_at(0, now, || None);
        assert!(
            cache
                .read_at(0, now + Duration::from_secs(4), || panic!("early retry"))
                .is_none()
        );
        assert!(
            cache
                .read_at(0, now + Duration::from_secs(5), sample)
                .is_some()
        );
    }
}
