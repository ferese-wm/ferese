//! Input estimation and destination selection; all post-release motion uses springs.
use std::{collections::VecDeque, time::Duration};

#[derive(Default, Debug)]
pub struct VelocityTracker {
    samples: VecDeque<(Duration, f64)>,
}

impl VelocityTracker {
    pub fn push(&mut self, time: Duration, position: f64) {
        if !position.is_finite() {
            return;
        }
        if let Some(&(last, _)) = self.samples.back() {
            if time < last {
                return;
            }
            if time == last {
                self.samples.pop_back();
            }
        }
        self.samples.push_back((time, position));
        while self
            .samples
            .front()
            .is_some_and(|(t, _)| time.saturating_sub(*t) > Duration::from_millis(100))
            || self.samples.len() > 32
        {
            self.samples.pop_front();
        }
    }

    pub fn velocity(&self, now: Duration) -> f64 {
        let Some(&(last, origin)) = self.samples.back() else {
            return 0.0;
        };
        if now.saturating_sub(last) > Duration::from_millis(40) || self.samples.len() < 2 {
            return 0.0;
        }
        // Fit positions relative to the newest sample, with time normalized to
        // the history horizon to avoid ill-conditioned powers of tiny seconds.
        let degree = if self.samples.len() >= 3 { 2 } else { 1 };
        let mut matrix = [[0.0; 4]; 3];
        for &(time, position) in &self.samples {
            let t = -last.saturating_sub(time).as_secs_f64() / 0.1;
            let powers = [1.0, t, t * t, t * t * t, t * t * t * t];
            for row in 0..=degree {
                for col in 0..=degree {
                    matrix[row][col] += powers[row + col];
                }
                matrix[row][degree + 1] += powers[row] * (position - origin);
            }
        }
        for col in 0..=degree {
            let pivot = (col..=degree)
                .max_by(|&a, &b| matrix[a][col].abs().total_cmp(&matrix[b][col].abs()))
                .unwrap();
            matrix.swap(col, pivot);
            let divisor = matrix[col][col];
            if divisor.abs() < 1e-12 {
                let (first, position) = self.samples[0];
                let dt = last.saturating_sub(first).as_secs_f64();
                return if dt > 0.0 { (origin - position) / dt } else { 0.0 };
            }
            for entry in &mut matrix[col][col..=degree + 1] {
                *entry /= divisor;
            }
            let pivot_row = matrix[col];
            for (row, values) in matrix.iter_mut().enumerate().take(degree + 1) {
                if row == col {
                    continue;
                }
                let factor = values[col];
                for (entry, pivot) in values[col..=degree + 1].iter_mut().zip(&pivot_row[col..=degree + 1]) {
                    *entry -= factor * pivot;
                }
            }
        }
        matrix[1][degree + 1] / 0.1
    }
}

pub fn project(position: f64, velocity: f64, rate: f64) -> f64 {
    if !velocity.is_finite() || !(0.0..1.0).contains(&rate) || rate == 0.0 {
        return position;
    }
    position - velocity / (1000.0 * rate.ln())
}

/// Nonlinear resistance, bounded by `extent`; the derivative transfers the
/// velocity of the displayed (resisted) position into the release spring.
pub fn rubber_band(distance: f64, extent: f64, resistance: f64) -> (f64, f64) {
    if !distance.is_finite() || !extent.is_finite() || extent <= 0.0 || !resistance.is_finite() || resistance <= 0.0 {
        return (0.0, 0.0);
    }
    let denominator = 1.0 + resistance * distance.abs() / extent;
    (
        distance.signum() * extent * (1.0 - denominator.recip()),
        resistance / denominator.powi(2),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rubber_band_is_bounded_symmetric_and_has_matching_velocity() {
        for x in [0.0, 1.0, 100.0, 10000.0] {
            let (position, derivative) = rubber_band(x, 64.0, 0.55);
            assert!((0.0..64.0).contains(&position));
            assert_eq!(rubber_band(-x, 64.0, 0.55).0, -position);
            let measured = (rubber_band(x + 0.0001, 64.0, 0.55).0 - rubber_band(x - 0.0001, 64.0, 0.55).0) / 0.0002;
            assert!((measured - derivative).abs() < 1e-6);
        }
    }
    #[test]
    fn quadratic_velocity_and_pause() {
        let mut tracker = VelocityTracker::default();
        for ms in [0, 7, 19, 40, 60, 90] {
            let t = ms as f64 / 1000.0;
            tracker.push(Duration::from_millis(ms), 100.0 * t + 200.0 * t * t);
        }
        assert!((tracker.velocity(Duration::from_millis(90)) - 136.0).abs() < 1e-8);
        assert_eq!(tracker.velocity(Duration::from_millis(140)), 0.0);
        assert!((project(0.0, 1.0, 0.997) - 0.332833082957).abs() < 1e-10);
    }
    #[test]
    fn duplicate_and_out_of_order_samples() {
        let mut tracker = VelocityTracker::default();
        tracker.push(Duration::ZERO, 0.0);
        tracker.push(Duration::from_millis(10), 1.0);
        tracker.push(Duration::from_millis(10), 2.0);
        tracker.push(Duration::from_millis(5), 99.0);
        assert!((tracker.velocity(Duration::from_millis(10)) - 200.0).abs() < 1e-10);
    }
}
