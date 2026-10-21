use std::time::Duration;

use crate::PanelStyle;

pub const HYSTERESIS: Duration = Duration::from_millis(150);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowState {
    pub maximized: bool,
    pub fullscreen: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presentation {
    Floating,
    Flat,
    Hidden,
}

impl Presentation {
    #[must_use]
    pub fn style(self) -> Option<PanelStyle> {
        match self {
            Self::Floating => Some(PanelStyle::Floating),
            Self::Flat => Some(PanelStyle::Flat),
            Self::Hidden => None,
        }
    }
}

impl From<WindowState> for Presentation {
    fn from(state: WindowState) -> Self {
        if state.fullscreen {
            Self::Hidden
        } else if state.maximized {
            Self::Flat
        } else {
            Self::Floating
        }
    }
}

/// One instance belongs to one output. A deadline exists only while a new
/// ordinary window state remains stable; fullscreen takes effect immediately.
#[derive(Clone, Debug)]
pub struct Adaptive {
    current: Presentation,
    pending: Option<(Presentation, Duration)>,
}

impl Adaptive {
    #[must_use]
    pub fn new(state: WindowState) -> Self {
        Self {
            current: state.into(),
            pending: None,
        }
    }

    #[must_use]
    pub fn current(&self) -> Presentation {
        self.current
    }

    pub fn observe(&mut self, state: WindowState, now: Duration) -> bool {
        let desired = Presentation::from(state);
        if desired == self.current {
            self.pending = None;
            return false;
        }
        if desired == Presentation::Hidden || self.current == Presentation::Hidden {
            self.current = desired;
            self.pending = None;
            return true;
        }
        match self.pending {
            Some((target, since)) if target == desired && now >= since => {
                if now - since >= HYSTERESIS {
                    self.current = target;
                    self.pending = None;
                    true
                } else {
                    false
                }
            }
            _ => {
                self.pending = Some((desired, now));
                false
            }
        }
    }

    #[must_use]
    pub fn deadline(&self) -> Option<Duration> {
        self.pending
            .map(|(_, since)| since.saturating_add(HYSTERESIS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }
    fn maximized() -> WindowState {
        WindowState {
            maximized: true,
            fullscreen: false,
        }
    }

    #[test]
    fn boundary_flapping_cancels_the_pending_style() {
        let mut state = Adaptive::new(WindowState::default());
        assert!(!state.observe(maximized(), at(0)));
        assert!(!state.observe(WindowState::default(), at(149)));
        assert_eq!(state.deadline(), None);
        assert!(!state.observe(maximized(), at(150)));
        assert!(!state.observe(maximized(), at(299)));
        assert!(state.observe(maximized(), at(300)));
        assert_eq!(state.current(), Presentation::Flat);
    }

    #[test]
    fn fullscreen_cancels_pending_work_and_restores_without_delay() {
        let mut state = Adaptive::new(WindowState::default());
        state.observe(maximized(), at(0));
        assert!(state.observe(
            WindowState {
                fullscreen: true,
                ..maximized()
            },
            at(1)
        ));
        assert_eq!(state.current().style(), None);
        assert_eq!(state.deadline(), None);
        assert!(state.observe(maximized(), at(2)));
        assert_eq!(state.current().style(), Some(PanelStyle::Flat));
    }

    #[test]
    fn outputs_change_independently() {
        let mut left = Adaptive::new(WindowState::default());
        let right = Adaptive::new(WindowState::default());
        left.observe(maximized(), at(0));
        left.observe(maximized(), at(150));
        assert_eq!(left.current(), Presentation::Flat);
        assert_eq!(right.current(), Presentation::Floating);
    }
}
