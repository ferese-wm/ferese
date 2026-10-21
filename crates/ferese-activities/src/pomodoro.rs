//! One session's timer. The host supplies CLOCK_BOOTTIME samples and delivers
//! returned completions; neither presentation nor client connections own time.
use crate::Time;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub work_minutes: u16,
    pub short_break_minutes: u16,
    pub long_break_minutes: u16,
    pub intervals: u8,
    pub auto_advance: bool,
    pub notifications: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            work_minutes: 25,
            short_break_minutes: 5,
            long_break_minutes: 15,
            intervals: 4,
            auto_advance: false,
            notifications: true,
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1..=180).contains(&self.work_minutes)
            || !(1..=60).contains(&self.short_break_minutes)
            || !(1..=120).contains(&self.long_break_minutes)
            || !(1..=12).contains(&self.intervals)
        {
            return Err(
                "Pomodoro requires work 1–180 min, short break 1–60 min, long break 1–120 min, and 1–12 intervals",
            );
        }
        Ok(())
    }
    fn duration(&self, phase: Phase) -> u64 {
        u64::from(match phase {
            Phase::Work => self.work_minutes,
            Phase::ShortBreak => self.short_break_minutes,
            Phase::LongBreak => self.long_break_minutes,
        }) * 60_000
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    Work,
    ShortBreak,
    LongBreak,
}
impl Phase {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Work => "Work",
            Self::ShortBreak => "Short break",
            Self::LongBreak => "Long break",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Running,
    Paused,
    Ready,
    Finished,
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Session {
    pub settings: Settings,
    pub phase: Phase,
    pub status: Status,
    pub completed_work: u8,
    pub remaining_ms: u64,
    pub deadline: Option<Time>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Completion {
    pub phase: Phase,
    pub next: Option<Phase>,
}
impl Session {
    pub fn start(settings: Settings, now: Time) -> Result<Self, &'static str> {
        settings.validate()?;
        let remaining_ms = settings.duration(Phase::Work);
        Ok(Self {
            settings,
            phase: Phase::Work,
            status: Status::Running,
            completed_work: 0,
            remaining_ms,
            deadline: Some(now.saturating_add(remaining_ms)),
        })
    }
    #[must_use]
    pub fn remaining(&self, now: Time) -> u64 {
        self.deadline
            .map_or(self.remaining_ms, |deadline| deadline.saturating_sub(now))
    }
    fn next_phase(&self) -> Option<Phase> {
        match self.phase {
            Phase::Work if self.completed_work >= self.settings.intervals => Some(Phase::LongBreak),
            Phase::Work => Some(Phase::ShortBreak),
            Phase::ShortBreak => Some(Phase::Work),
            Phase::LongBreak => None,
        }
    }
    /// Completes at most the interval that was actually running. After suspend,
    /// automatic advancement starts the next interval at this observation, rather
    /// than counting unattended work intervals during sleep.
    pub fn advance(&mut self, now: Time) -> Option<Completion> {
        if self.deadline.is_none_or(|deadline| now < deadline) {
            return None;
        }
        if self.phase == Phase::Work {
            self.completed_work += 1;
        }
        let completion = Completion {
            phase: self.phase,
            next: self.next_phase(),
        };
        self.select(completion.next, now, self.settings.auto_advance);
        Some(completion)
    }
    fn select(&mut self, phase: Option<Phase>, now: Time, running: bool) {
        self.deadline = None;
        if let Some(phase) = phase {
            self.phase = phase;
            self.remaining_ms = self.settings.duration(phase);
            self.status = if running {
                Status::Running
            } else {
                Status::Ready
            };
            if running {
                self.deadline = Some(now.saturating_add(self.remaining_ms));
            }
        } else {
            self.remaining_ms = 0;
            self.status = Status::Finished;
        }
    }
    pub fn pause(&mut self, now: Time) -> Result<(), &'static str> {
        match self.status {
            Status::Paused => Ok(()),
            Status::Running => {
                self.remaining_ms = self.remaining(now);
                self.deadline = None;
                self.status = Status::Paused;
                Ok(())
            }
            _ => Err("No running Pomodoro interval"),
        }
    }
    pub fn resume(&mut self, now: Time) -> Result<(), &'static str> {
        match self.status {
            Status::Running => Ok(()),
            Status::Paused | Status::Ready => {
                self.deadline = Some(now.saturating_add(self.remaining_ms));
                self.status = Status::Running;
                Ok(())
            }
            Status::Finished => Err("Start a new Pomodoro session"),
        }
    }
    /// Skipping never completes work or emits a completion. A Ready interval is
    /// already the next phase, so Next starts it rather than skipping twice.
    pub fn next(&mut self, now: Time) -> Result<(), &'static str> {
        match self.status {
            Status::Finished => Err("Start a new Pomodoro session"),
            Status::Ready => self.resume(now),
            _ => {
                self.select(self.next_phase(), now, true);
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pauses_preserve_milliseconds_and_do_not_count_sleep() {
        let mut session = Session::start(Settings::default(), 100).unwrap();
        session.pause(1_337).unwrap();
        assert_eq!(session.remaining(100_000_000), 1_498_763);
        assert!(session.advance(100_000_000).is_none());
        session.resume(100_000_000).unwrap();
        assert_eq!(session.deadline, Some(101_498_763));
        session.pause(100_000_001).unwrap();
        assert_eq!(session.remaining_ms, 1_498_762);
        assert_eq!(session.completed_work, 0);
    }
    #[test]
    fn boundaries_and_long_break_are_exactly_once() {
        let mut session = Session::start(
            Settings {
                intervals: 2,
                ..Default::default()
            },
            0,
        )
        .unwrap();
        for (finished, next, count) in [
            (Phase::Work, Some(Phase::ShortBreak), 1),
            (Phase::ShortBreak, Some(Phase::Work), 1),
            (Phase::Work, Some(Phase::LongBreak), 2),
            (Phase::LongBreak, None, 2),
        ] {
            let deadline = session.deadline.unwrap();
            assert!(session.advance(deadline - 1).is_none());
            assert_eq!(
                session.advance(deadline),
                Some(Completion {
                    phase: finished,
                    next
                })
            );
            assert_eq!(session.completed_work, count);
            assert!(session.advance(deadline + 99_000_000).is_none());
            if next.is_some() {
                assert_eq!(session.status, Status::Ready);
                session.next(deadline).unwrap();
            }
        }
        assert_eq!(session.status, Status::Finished);
        assert!(session.resume(0).is_err());
    }
    #[test]
    fn skipping_and_cancelling_never_complete_work() {
        let mut session = Session::start(Settings::default(), 0).unwrap();
        session.next(50).unwrap();
        assert_eq!(session.phase, Phase::ShortBreak);
        session.next(60).unwrap();
        assert_eq!(session.phase, Phase::Work);
        assert_eq!(session.completed_work, 0);
        session.pause(70).unwrap();
        session.next(80).unwrap();
        assert_eq!(session.completed_work, 0);
    }
    #[test]
    fn suspend_finishes_only_the_running_interval() {
        let mut session = Session::start(
            Settings {
                auto_advance: true,
                ..Default::default()
            },
            0,
        )
        .unwrap();
        assert_eq!(session.advance(100_000_000).unwrap().phase, Phase::Work);
        assert_eq!(session.completed_work, 1);
        assert_eq!(session.deadline, Some(100_300_000));
        assert!(session.advance(100_000_000).is_none());
    }
    #[test]
    fn invalid_settings_are_rejected() {
        for settings in [
            Settings {
                work_minutes: 0,
                ..Default::default()
            },
            Settings {
                short_break_minutes: 61,
                ..Default::default()
            },
            Settings {
                long_break_minutes: 121,
                ..Default::default()
            },
            Settings {
                intervals: 13,
                ..Default::default()
            },
        ] {
            assert!(Session::start(settings, 0).is_err());
        }
    }
}
