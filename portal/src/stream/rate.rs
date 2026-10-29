use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use pipewire::spa::utils::Fraction;

/// The negotiated capture clock, independent of the output's refresh rate.
pub(super) struct FrameRate {
    current: AtomicU64,
    pub(super) maximum: u32,
}

impl FrameRate {
    pub(super) fn try_new(value: Option<&str>) -> Result<Self, String> {
        let maximum = match value {
            None => 60,
            Some(value) => value
                .parse::<u32>()
                .ok()
                .filter(|value| (1..=240).contains(value))
                .ok_or("FERESE_CAPTURE_MAX_FPS must be an integer from 1 to 240")?,
        };
        Ok(Self {
            current: AtomicU64::new((u64::from(maximum.min(30)) << 32) | 1),
            maximum,
        })
    }

    pub(super) fn negotiate(&self, rate: Fraction) -> Result<(), &'static str> {
        if rate.denom == 0
            || rate.num < rate.denom
            || u64::from(rate.num) > u64::from(self.maximum) * u64::from(rate.denom)
        {
            return Err("Negotiated capture rate is outside the advertised range");
        }
        self.current.store(
            (u64::from(rate.num) << 32) | u64::from(rate.denom),
            Ordering::Relaxed,
        );
        Ok(())
    }

    pub(super) fn fraction(&self) -> Fraction {
        let packed = self.current.load(Ordering::Relaxed);
        Fraction {
            num: (packed >> 32) as u32,
            denom: packed as u32,
        }
    }

    pub(super) fn interval(&self) -> Duration {
        let rate = self.fraction();
        Duration::from_nanos(1_000_000_000 * u64::from(rate.denom) / u64::from(rate.num))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configuration_bounds_and_default_clock() {
        let rate = FrameRate::try_new(None).unwrap();
        assert_eq!(rate.maximum, 60);
        assert_eq!(rate.interval(), Duration::from_nanos(33_333_333));
        assert_eq!(FrameRate::try_new(Some("15")).unwrap().fraction().num, 15);
        for invalid in ["", "0", "241", "-1", "60.0", " 60"] {
            assert!(FrameRate::try_new(Some(invalid)).is_err(), "{invalid}");
        }
    }

    #[test]
    fn consumer_negotiation_preserves_fractional_rates() {
        let rate = FrameRate::try_new(None).unwrap();
        rate.negotiate(Fraction {
            num: 60_000,
            denom: 1001,
        })
        .unwrap();
        assert_eq!(rate.interval(), Duration::from_nanos(16_683_333));
        for (num, denom) in [(0, 1), (1, 0), (1, 2), (61, 1), (u32::MAX, 1)] {
            assert!(rate.negotiate(Fraction { num, denom }).is_err());
            assert_eq!(rate.fraction().num, 60_000);
        }
    }
}
