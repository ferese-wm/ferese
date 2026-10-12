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

/// Open border paths for screen-attached notches. Split shared straight edges
/// before cancelling them, so the body and its fillets have one continuous rim.
pub fn notch_border(scene: &Scene, tolerance: f64, edge: crate::Edge) -> Result<Vec<Contour>, &'static str> {
    let contours = contours(scene, tolerance)?;
    let vertices: Vec<_> = contours.iter().flatten().copied().collect();
    let screen_edge = vertices.iter().map(|p| p[1]).fold(
        if edge == crate::Edge::Top {
            f64::INFINITY
        } else {
            f64::NEG_INFINITY
        },
        if edge == crate::Edge::Top { f64::min } else { f64::max },
    );
    let close = |a: [f64; 2], b: [f64; 2]| (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6;
    let mut segments: Vec<([f64; 2], [f64; 2])> = Vec::new();
    for contour in &contours {
        for (&a, &b) in contour.iter().zip(contour.iter().cycle().skip(1)).take(contour.len()) {
            if close(a, b) || ((a[1] - screen_edge).abs() < 1e-6 && (b[1] - screen_edge).abs() < 1e-6) {
                continue;
            }
            let delta = [b[0] - a[0], b[1] - a[1]];
            let length_squared = delta[0] * delta[0] + delta[1] * delta[1];
            let mut cuts = vec![(0., a), (1., b)];
            for &point in &vertices {
                let offset = [point[0] - a[0], point[1] - a[1]];
                let t = (offset[0] * delta[0] + offset[1] * delta[1]) / length_squared;
                if t > 0. && t < 1. && (offset[0] * delta[1] - offset[1] * delta[0]).abs() < 1e-6 {
                    cuts.push((t, point));
                }
            }
            cuts.sort_by(|a, b| a.0.total_cmp(&b.0));
            cuts.dedup_by(|a, b| close(a.1, b.1));
            for pair in cuts.windows(2) {
                let segment = (pair[0].1, pair[1].1);
                if let Some(index) = segments
                    .iter()
                    .position(|&(a, b)| close(a, segment.1) && close(b, segment.0))
                {
                    segments.swap_remove(index);
                } else {
                    segments.push(segment);
                }
            }
        }
    }
    let mut paths = Vec::new();
    while let Some((a, b)) = segments.pop() {
        let mut path = vec![a, b];
        while let Some(index) = segments.iter().position(|&(a, _)| close(a, *path.last().unwrap())) {
            path.push(segments.swap_remove(index).1);
        }
        while let Some(index) = segments.iter().position(|&(_, b)| close(b, path[0])) {
            path.insert(0, segments.swap_remove(index).0);
        }
        paths.push(path);
    }
    Ok(paths)
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
    fn notch_rim_connects_both_fillets_without_shared_or_screen_edges() {
        for scale in [1., 1.25, 1.5, 2.] {
            for edge in [crate::Edge::Top, crate::Edge::Bottom] {
                let scene = derive(
                    Parameters {
                        style: Style::Notch,
                        edge,
                        height: 44.,
                        fillet: 12.,
                        ..Default::default()
                    },
                    Rect::new(0., 0., 340., 44.),
                    &[GroupBounds {
                        zone: ZonePosition::Center,
                        bounds: Rect::new(70.3, 0., 160.4, 44.),
                    }],
                    PixelGrid::new(scale).unwrap(),
                )
                .unwrap();
                let paths = notch_border(&scene, 0.1 / scale, edge).unwrap();
                assert_eq!(paths.len(), 1, "{edge:?} at {scale}");
                let path = &paths[0];
                let screen_y = if edge == crate::Edge::Top { 0. } else { 44. };
                assert!((path.first().unwrap()[1] - screen_y).abs() < 1e-6);
                assert!((path.last().unwrap()[1] - screen_y).abs() < 1e-6);
                let body = scene.paint.iter().find(|p| p.operation == 0).unwrap().bounds;
                assert!(path.iter().any(|p| p[0] < body.x));
                assert!(path.iter().any(|p| p[0] > body.x + body.width));
                for segment in path.windows(2) {
                    let [a, b] = [segment[0], segment[1]];
                    assert!((a[1] - screen_y).abs() > 1e-6 || (b[1] - screen_y).abs() > 1e-6);
                    let mid = [(a[0] + b[0]) / 2., (a[1] + b[1]) / 2.];
                    assert!(scene.distance(mid).abs() < 0.12 / scale, "off outline: {mid:?}");
                    // A shared body/fillet seam has fill on both sides, unlike a visible rim.
                    let delta = [b[0] - a[0], b[1] - a[1]];
                    let length = delta[0].hypot(delta[1]);
                    let normal = [-delta[1] / length * 0.25, delta[0] / length * 0.25];
                    assert!(
                        scene.distance([mid[0] + normal[0], mid[1] + normal[1]]) > 0.
                            || scene.distance([mid[0] - normal[0], mid[1] - normal[1]]) > 0.
                    );
                }
            }
        }
    }

    #[test]
    fn attached_morph_rim_follows_animated_body_at_fractional_scales() {
        for scale in [1., 1.25, 1.5, 2.] {
            for edge in [crate::Edge::Top, crate::Edge::Bottom] {
                for (width, height, radius) in [(150., 34., 17.), (218.3, 41.7, 20.85), (340., 230., 26.)] {
                    let scene = derive(
                        Parameters {
                            style: Style::Morph,
                            attached: true,
                            edge,
                            fillet: 12.,
                            radius: Some(crate::shape::AnchoredRadii([radius; 4])),
                            ..Default::default()
                        },
                        Rect::new(30.3, 0., width, height),
                        &[],
                        PixelGrid::new(scale).unwrap(),
                    )
                    .unwrap();
                    let body = scene.paint[0];
                    assert_eq!(scene.paint.len(), 3);
                    assert_eq!(scene.input.len(), 1, "decorative shoulders must not take pointer input");
                    let screen_y = if edge == crate::Edge::Top {
                        body.bounds.y
                    } else {
                        body.bounds.y + body.bounds.height
                    };
                    let paths = notch_border(&scene, 0.1 / scale, edge).unwrap();
                    assert_eq!(
                        paths.len(),
                        1,
                        "one continuous rim: {scale}, {edge:?}, {width}, {height}"
                    );
                    let path = &paths[0];
                    assert!((path.first().unwrap()[1] - screen_y).abs() < 1e-6);
                    assert!((path.last().unwrap()[1] - screen_y).abs() < 1e-6);
                    assert!(path.iter().any(|p| p[0] < body.bounds.x));
                    assert!(path.iter().any(|p| p[0] > body.bounds.x + body.bounds.width));
                    for segment in path.windows(2) {
                        let [a, b] = [segment[0], segment[1]];
                        assert!(
                            (a[1] - screen_y).abs() > 1e-6 || (b[1] - screen_y).abs() > 1e-6,
                            "no stroke on the screen edge"
                        );
                        let mid = [(a[0] + b[0]) / 2., (a[1] + b[1]) / 2.];
                        assert!(scene.distance(mid).abs() < 0.12 / scale);
                    }
                    let contours = contours(&scene, 0.1 / scale).unwrap();
                    for x in 0..((width + 60.) as usize) {
                        for y in 0..(height as usize) {
                            let p = [x as f64 + 0.37, y as f64 + 0.37];
                            if scene.distance(p).abs() > 0.15 / scale {
                                assert_eq!(
                                    winding(&contours, p) != 0,
                                    scene.distance(p) < 0.,
                                    "native and fallback must agree"
                                );
                            }
                        }
                    }
                }
            }
        }
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
