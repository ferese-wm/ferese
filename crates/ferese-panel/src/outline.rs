use crate::scene::{Primitive, Scene, smooth_min};
use std::f64::consts::{FRAC_PI_2, PI};

pub type Contour = Vec<[f64; 2]>;

fn arc(points: &mut Contour, center: [f64; 2], radius: f64, start: f64, tolerance: f64) {
    let segments = ((FRAC_PI_2 / (8. * tolerance / radius.max(tolerance)).sqrt()).ceil() as usize).clamp(1, 128);
    for i in 0..=segments {
        let angle = start + FRAC_PI_2 * i as f64 / segments as f64;
        points.push([center[0] + radius * angle.cos(), center[1] + radius * angle.sin()]);
    }
}

fn clockwise(points: &mut Contour, cutout: bool) {
    let area = points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .take(points.len())
        .map(|(a, b)| a[0] * b[1] - b[0] * a[1])
        .sum::<f64>();
    if (area < 0.) != cutout {
        points.reverse();
    }
}

fn primitive_outline(primitive: Primitive, tolerance: f64) -> Contour {
    let b = primitive.bounds;
    let (l, t, r, d) = (b.x, b.y, b.x + b.width, b.y + b.height);
    let mut points = Vec::new();
    if primitive.operation >= 2 {
        let radius = b.width.min(b.height);
        let (center, start, remaining) = match primitive.operation {
            2 => ([l, d], -FRAC_PI_2, [[r, d], [r, t], [l, t]]),
            3 => ([r, d], PI, [[r, t], [l, t], [l, d]]),
            4 => ([r, t], FRAC_PI_2, [[l, t], [l, d], [r, d]]),
            _ => ([l, t], 0., [[l, d], [r, d], [r, t]]),
        };
        arc(&mut points, center, radius, start, tolerance);
        points.extend(remaining);
    } else {
        let [tl, tr, br, bl] = primitive.radii.0.map(f64::from);
        for (center, radius, start) in [
            ([l + tl, t + tl], tl, PI),
            ([r - tr, t + tr], tr, PI + FRAC_PI_2),
            ([r - br, d - br], br, 0.),
            ([l + bl, d - bl], bl, FRAC_PI_2),
        ] {
            arc(&mut points, center, radius, start, tolerance);
        }
    }
    clockwise(&mut points, primitive.operation == 1);
    points
}

/// Paths for vector backends. Additive contours share one nonzero fill, so
/// touching groups and fillets have no separately anti-aliased seam.
pub fn contours(scene: &Scene, tolerance: f64) -> Result<Vec<Contour>, &'static str> {
    if !tolerance.is_finite() || !(0.01..=1.).contains(&tolerance) {
        return Err("outline tolerance must be between 0.01 and 1 logical pixels");
    }
    let mut result: Vec<_> = scene
        .paint
        .iter()
        .filter(|p| scene.fusion == 0. || p.blend_zone == 0)
        .map(|p| primitive_outline(*p, tolerance))
        .collect();
    if scene.fusion > 0. {
        for zone in 1..=3 {
            let shapes: Vec<_> = scene.paint.iter().filter(|p| p.blend_zone == zone).copied().collect();
            if shapes.is_empty() {
                continue;
            }
            // Fused rows share their vertical baseline. Each component is
            // vertically convex; solve its upper and lower boundary, rather
            // than sampling every pixel of a full-output bitmap.
            let top = shapes.iter().map(|p| p.bounds.y).fold(f64::INFINITY, f64::min);
            let bottom = shapes
                .iter()
                .map(|p| p.bounds.y + p.bounds.height)
                .fold(f64::NEG_INFINITY, f64::max);
            let mid = (top + bottom) * 0.5;
            let left = shapes.iter().map(|p| p.bounds.x).fold(f64::INFINITY, f64::min) - scene.fusion;
            let right = shapes
                .iter()
                .map(|p| p.bounds.x + p.bounds.width)
                .fold(f64::NEG_INFINITY, f64::max)
                + scene.fusion;
            let distance = |x, y| {
                shapes
                    .iter()
                    .map(|p| p.distance([x, y]))
                    .fold(f64::INFINITY, |a, b| smooth_min(a, b, scene.fusion))
            };
            let step = tolerance * 2.;
            let count = ((right - left) / step).ceil() as usize;
            if count > 1_000_000 {
                return Err("fused outline exceeds sampling capacity");
            }
            let mut upper = Vec::new();
            let mut lower = Vec::new();
            let finish = |upper: &mut Contour, lower: &mut Contour, result: &mut Vec<Contour>| {
                if !upper.is_empty() {
                    upper.extend(lower.drain(..).rev());
                    clockwise(upper, false);
                    result.push(std::mem::take(upper));
                }
            };
            for i in 0..=count {
                let x = left + (right - left) * i as f64 / count.max(1) as f64;
                if distance(x, mid) >= 0. {
                    finish(&mut upper, &mut lower, &mut result);
                    continue;
                }
                let root = |outside: f64| {
                    let mut a = outside;
                    let mut b = mid;
                    for _ in 0..16 {
                        let y = (a + b) * 0.5;
                        if distance(x, y) < 0. {
                            b = y;
                        } else {
                            a = y;
                        }
                    }
                    (a + b) * 0.5
                };
                upper.push([x, root(top - scene.fusion)]);
                lower.push([x, root(bottom + scene.fusion)]);
            }
            finish(&mut upper, &mut lower, &mut result);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{GroupBounds, Parameters, derive};
    use crate::shape::{PixelGrid, Style, ZonePosition};
    use ferese_layout::Rect;

    fn winding(contours: &[Contour], point: [f64; 2]) -> i32 {
        let mut winding = 0;
        for points in contours {
            for (a, b) in points.iter().zip(points.iter().cycle().skip(1)).take(points.len()) {
                let cross = (b[0] - a[0]) * (point[1] - a[1]) - (point[0] - a[0]) * (b[1] - a[1]);
                if a[1] <= point[1] && b[1] > point[1] && cross > 0. {
                    winding += 1;
                }
                if a[1] > point[1] && b[1] <= point[1] && cross < 0. {
                    winding -= 1;
                }
            }
        }
        winding
    }

    #[test]
    fn vector_union_and_cutout_match_implicit_shapes() {
        let groups = [
            GroupBounds {
                zone: ZonePosition::Start,
                bounds: Rect::new(4., 0., 20., 16.),
            },
            GroupBounds {
                zone: ZonePosition::Start,
                bounds: Rect::new(28., 0., 20., 16.),
            },
            GroupBounds {
                zone: ZonePosition::Center,
                bounds: Rect::new(52., 0., 20., 16.),
            },
        ];
        for style in [
            Style::Floating,
            Style::Capsules,
            Style::Notch,
            Style::Flat,
            Style::Frame,
            Style::CornerWrap,
            Style::Fused,
        ] {
            let scene = derive(
                Parameters {
                    style,
                    height: 16.,
                    gap: 4.,
                    frame_insets: [16., 4., 4., 4.],
                    frame_radius: 8.,
                    wrap_size: [40., 42.],
                    wrap_width: 8.,
                    fillet: 6.,
                    ..Default::default()
                },
                Rect::new(0., 0., 96., 64.),
                &groups,
                PixelGrid::new(1.25).unwrap(),
            )
            .unwrap();
            let contours = contours(&scene, 0.1).unwrap();
            for y in 0..64 {
                for x in 0..96 {
                    let point = [x as f64 + 0.5, y as f64 + 0.5];
                    let distance = scene.distance(point);
                    if distance.abs() > 0.3 {
                        assert_eq!(
                            winding(&contours, point) != 0,
                            distance < 0.,
                            "{style:?} at {point:?}, {distance}"
                        );
                    }
                }
            }
        }
    }
}
