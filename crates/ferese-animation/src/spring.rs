use super::SpringConfig;

/// Solve m y'' + c y' + k y = 0, with y = current - target.
/// No settling tolerance or crossing clamp is applied here.
pub(super) fn solve(current: f64, velocity: f64, target: f64, dt: f64, config: SpringConfig) -> (f64, f64) {
    if dt <= 0.0 || !dt.is_finite() {
        return (current, velocity);
    }

    let config = config.normalized();
    let omega2 = config.stiffness / config.mass;
    let omega = omega2.sqrt();
    let a = config.damping / (2.0 * config.mass);
    let d = (a - omega) * (a + omega);
    let y = current - target;
    let z2 = d * dt * dt;

    // cos/sinc and cosh/sinhc have the same power series in signed z².
    // This keeps the actual discriminant, including near-critical damping,
    // rather than replacing a neighborhood with the critical solution.
    if z2.abs() < 0.01 {
        let c = 1.0 + z2 * (0.5 + z2 * (1.0 / 24.0 + z2 * (1.0 / 720.0 + z2 / 40320.0)));
        let s = dt * (1.0 + z2 * (1.0 / 6.0 + z2 * (1.0 / 120.0 + z2 * (1.0 / 5040.0 + z2 / 362880.0))));
        let e = (-a * dt).exp();
        return (
            target + e * (y * c + (velocity + a * y) * s),
            e * (velocity * c - (a * velocity + omega2 * y) * s),
        );
    }

    if d < 0.0 {
        let w = (-d).sqrt();
        let (sin, cos) = (w * dt).sin_cos();
        let e = (-a * dt).exp();
        let s = sin / w;
        (
            target + e * (y * cos + (velocity + a * y) * s),
            e * (velocity * cos - (a * velocity + omega2 * y) * s),
        )
    } else {
        // Avoid -a + sqrt(a²-omega²) cancellation and exp/cosh overflow.
        let q = d.sqrt();
        let fast = -a - q;
        let slow = -omega2 / (a + q);
        let slow_coefficient = (velocity - fast * y) / (2.0 * q);
        let fast_coefficient = (slow * y - velocity) / (2.0 * q);
        let slow_term = slow_coefficient * (slow * dt).exp();
        let fast_term = fast_coefficient * (fast * dt).exp();
        (target + slow_term + fast_term, slow * slow_term + fast * fast_term)
    }
}

/// True if the physical trajectory reaches equilibrium anywhere in (0, dt].
/// Endpoint-only tests miss an even number of crossings after a long stall.
pub(super) fn crosses(current: f64, velocity: f64, target: f64, dt: f64, config: SpringConfig) -> bool {
    if dt <= 0.0 {
        return false;
    }

    let config = config.normalized();
    let a = config.damping / (2.0 * config.mass);
    let omega = (config.stiffness / config.mass).sqrt();
    let d = (a - omega) * (a + omega);
    let y = current - target;
    let b = velocity + a * y;
    if y == 0.0 && velocity == 0.0 {
        return false;
    }

    let crossing = if d < 0.0 {
        let w = (-d).sqrt();
        let phase = (-y * w).atan2(b).rem_euclid(std::f64::consts::PI);
        let phase = if phase == 0.0 { std::f64::consts::PI } else { phase };
        phase / w
    } else if y == 0.0 || b == 0.0 {
        return false;
    } else {
        let t = -y / b;
        if t <= 0.0 {
            return false;
        }

        // atanh(q*t)/q = t * (1 + (q*t)²/3 + ...), including q = 0.
        let z2 = d * t * t;
        if z2 < 1e-8 {
            t * (1.0 + z2 * (1.0 / 3.0 + z2 / 5.0))
        } else if z2 < 1.0 {
            let q = d.sqrt();
            (q * t).atanh() / q
        } else {
            return false;
        }
    };
    crossing <= dt
}
