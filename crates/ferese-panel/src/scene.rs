use crate::shape::{AnchoredRadii, PixelGrid, RoundedSurface, Style, ZonePosition};
use crate::{CornerRadii, Edge};
use ferese_layout::Rect;

pub const MAX_PRIMITIVES: usize = 32;

/// Additive rounded rectangles, one optional cutout, and concave corner patches.
/// Patch codes name the circle's center: bottom-left, bottom-right, top-right,
/// top-left. A patch is its rectangle minus that circle, rather than a hot area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Primitive {
    pub bounds: Rect,
    pub radii: CornerRadii,
    pub operation: u8,
    pub blend_zone: u8,
}

impl Primitive {
    pub fn packet(self) -> [f32; 10] {
        let Rect { x, y, width, height } = self.bounds;
        let [tl, tr, br, bl] = self.radii.0;
        [
            x as f32,
            y as f32,
            width as f32,
            height as f32,
            tl,
            tr,
            br,
            bl,
            self.operation as f32,
            self.blend_zone as f32,
        ]
    }

    pub fn from_packet(values: [f32; 10]) -> Result<Self, &'static str> {
        if !values.into_iter().all(|v| v.is_finite() && v.abs() <= 32768.)
            || values[2] <= 0.
            || values[3] <= 0.
            || values[4..8].iter().any(|v| *v < 0.)
            || values[8].fract() != 0.
            || !(0. ..=5.).contains(&values[8])
            || values[9].fract() != 0.
            || !(0. ..=3.).contains(&values[9])
        {
            return Err("invalid panel shape primitive");
        }
        Ok(Self {
            bounds: Rect::new(values[0].into(), values[1].into(), values[2].into(), values[3].into()),
            radii: CornerRadii([values[4], values[5], values[6], values[7]]).normalized(values[2], values[3]),
            operation: values[8] as u8,
            blend_zone: values[9] as u8,
        })
    }

    pub fn distance(self, point: [f64; 2]) -> f64 {
        let rect = rectangle_distance(point, self.bounds);
        if self.operation >= 2 {
            let b = self.bounds;
            let center = match self.operation {
                2 => [b.x, b.y + b.height],
                3 => [b.x + b.width, b.y + b.height],
                4 => [b.x + b.width, b.y],
                _ => [b.x, b.y],
            };
            return rect.max(b.width.min(b.height) - (point[0] - center[0]).hypot(point[1] - center[1]));
        }
        let b = self.bounds;
        let (x, y) = (point[0] - b.x, point[1] - b.y);
        let mut distance = rect;
        for (r, dx, dy) in [
            (self.radii.0[0], x, y),
            (self.radii.0[1], b.width - x, y),
            (self.radii.0[2], b.width - x, b.height - y),
            (self.radii.0[3], x, b.height - y),
        ] {
            let r = f64::from(r);
            if r > 0. && dx < r && dy < r {
                distance = distance.max((dx - r).hypot(dy - r) - r);
            }
        }
        distance
    }
}

fn rectangle_distance(point: [f64; 2], b: Rect) -> f64 {
    let x = (point[0] - b.x - b.width / 2.).abs() - b.width / 2.;
    let y = (point[1] - b.y - b.height / 2.).abs() - b.height / 2.;
    x.max(0.).hypot(y.max(0.)) + x.max(y).min(0.)
}

pub fn smooth_min(a: f64, b: f64, strength: f64) -> f64 {
    if strength <= 0. {
        return a.min(b);
    }
    let h = ((strength - (a - b).abs()) / strength).max(0.);
    a.min(b) - h * h * strength / 4.
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    pub paint: Vec<Primitive>,
    pub input: Vec<RoundedSurface>,
    /// Bounded drawing/capture regions. Frame uses strips, not the output area.
    pub damage: Vec<Rect>,
    pub dividers: Vec<[f64; 4]>,
    pub fusion: f64,
}

impl Scene {
    pub fn distance(&self, point: [f64; 2]) -> f64 {
        let mut zones = [f64::INFINITY; 4];
        let mut cutout = f64::INFINITY;
        for primitive in &self.paint {
            let distance = primitive.distance(point);
            if primitive.operation == 1 {
                cutout = cutout.min(distance);
            } else {
                let zone = usize::from(primitive.blend_zone);
                zones[zone] = if zone > 0 {
                    smooth_min(zones[zone], distance, self.fusion)
                } else {
                    zones[zone].min(distance)
                };
            }
        }
        zones.into_iter().fold(f64::INFINITY, f64::min).max(-cutout)
    }

    pub fn contains(&self, point: [f64; 2]) -> bool {
        self.input.iter().any(|shape| shape.contains(point))
    }

    pub fn packets(&self) -> Vec<[f32; 10]> {
        self.paint.iter().map(|p| p.packet()).collect()
    }

    /// Horizontal runs at physical-pixel centers. Corners and animated gaps
    /// stay click-through; adjacent equal runs are coalesced into one rectangle.
    pub fn input_rectangles(&self, scale: f64) -> Vec<[i32; 4]> {
        if !scale.is_finite() || scale <= 0. {
            return Vec::new();
        }
        let mut result: Vec<[i32; 4]> = Vec::new();
        for shape in &self.input {
            let b = shape.bounds();
            let left = (b.x * scale).floor() as i32;
            let right = ((b.x + b.width) * scale).ceil() as i32;
            let top = (b.y * scale).floor() as i32;
            let bottom = ((b.y + b.height) * scale).ceil() as i32;
            for y in top..bottom {
                let mut first = left;
                while first < right && !shape.contains([(f64::from(first) + 0.5) / scale, (f64::from(y) + 0.5) / scale])
                {
                    first += 1;
                }
                let mut last = right;
                while last > first && !shape.contains([(f64::from(last) - 0.5) / scale, (f64::from(y) + 0.5) / scale]) {
                    last -= 1;
                }
                if first == last {
                    continue;
                }
                if let Some(previous) = result.last_mut()
                    && previous[0] == first
                    && previous[2] == last - first
                    && previous[1] + previous[3] == y
                {
                    previous[3] += 1;
                } else {
                    result.push([first, y, last - first, 1]);
                }
            }
        }
        result
    }
}

#[derive(Clone, Copy, Debug)]
pub struct GroupBounds {
    pub zone: ZonePosition,
    pub bounds: Rect,
}

#[derive(Clone, Copy, Debug)]
pub struct Parameters {
    pub style: Style,
    pub attached: bool,
    pub edge: Edge,
    pub height: f64,
    pub gap: f64,
    pub radius: Option<AnchoredRadii>,
    pub frame_insets: [f64; 4],
    pub frame_radius: f64,
    pub wrap_size: [f64; 2],
    pub wrap_width: f64,
    pub fillet: f64,
}

impl Default for Parameters {
    fn default() -> Self {
        Self {
            style: Style::Flat,
            attached: false,
            edge: Edge::Top,
            height: 36.,
            gap: 8.,
            radius: None,
            frame_insets: [40., 8., 8., 8.],
            frame_radius: 14.,
            wrap_size: [560., 380.],
            wrap_width: 36.,
            fillet: 10.,
        }
    }
}

fn add(
    scene: &mut Scene,
    grid: PixelGrid,
    rect: Rect,
    radii: CornerRadii,
    input: bool,
    zone: u8,
) -> Result<(), &'static str> {
    let rounded = RoundedSurface::new(rect, radii, grid)?;
    if rounded.bounds().width == 0. || rounded.bounds().height == 0. {
        return Ok(());
    }
    scene.paint.push(Primitive {
        bounds: rounded.bounds(),
        radii: rounded.radii(),
        operation: 0,
        blend_zone: zone,
    });
    if input {
        scene.input.push(rounded);
    }
    scene.damage.push(rounded.bounds());
    Ok(())
}

pub fn derive(
    parameters: Parameters,
    bounds: Rect,
    groups: &[GroupBounds],
    grid: PixelGrid,
) -> Result<Scene, &'static str> {
    let bounds = grid.rect(bounds)?;
    if bounds.width == 0. || bounds.height == 0. {
        return Ok(Scene::default());
    }
    if ![
        parameters.height,
        parameters.gap,
        parameters.frame_radius,
        parameters.wrap_width,
        parameters.fillet,
    ]
    .into_iter()
    .chain(parameters.frame_insets)
    .chain(parameters.wrap_size)
    .all(|v| v.is_finite() && (0. ..=32768.).contains(&v))
        || parameters
            .radius
            .is_some_and(|r| r.screen_space(parameters.edge).validate().is_err())
    {
        return Err("invalid panel shape parameters");
    }
    let mut scene = Scene::default();
    match parameters.style {
        Style::Flat => add(&mut scene, grid, bounds, CornerRadii::uniform(0.), true, 0)?,
        Style::Frame => {
            add(&mut scene, grid, bounds, CornerRadii::uniform(0.), false, 0)?;
            let [top, right, bottom, left] = parameters.frame_insets;
            let left = left.min(bounds.width / 3.);
            let right = right.min(bounds.width / 3.);
            let top = top.min(bounds.height / 3.);
            let bottom = bottom.min(bounds.height / 3.);
            let inner = grid.rect(Rect::new(
                bounds.x + left,
                bounds.y + top,
                bounds.width - left - right,
                bounds.height - top - bottom,
            ))?;
            let r = parameters.frame_radius as f32;
            let radii = CornerRadii([
                if left > 0.5 && top > 0.5 { r } else { 0. },
                if right > 0.5 && top > 0.5 { r } else { 0. },
                if right > 0.5 && bottom > 0.5 { r } else { 0. },
                if left > 0.5 && bottom > 0.5 { r } else { 0. },
            ])
            .normalized(inner.width as f32, inner.height as f32);
            scene.paint.push(Primitive {
                bounds: inner,
                radii,
                operation: 1,
                blend_zone: 0,
            });
            scene.input.push(RoundedSurface::new(
                Rect::new(bounds.x, bounds.y, bounds.width, top),
                CornerRadii::uniform(0.),
                grid,
            )?);
            let radius = f64::from(radii.max());
            scene.damage = vec![
                Rect::new(bounds.x, bounds.y, bounds.width, top + radius),
                Rect::new(
                    bounds.x,
                    bounds.y + bounds.height - bottom - radius,
                    bounds.width,
                    bottom + radius,
                ),
                Rect::new(
                    bounds.x,
                    bounds.y + top + radius,
                    left + radius,
                    (bounds.height - top - bottom - 2. * radius).max(0.),
                ),
                Rect::new(
                    bounds.x + bounds.width - right - radius,
                    bounds.y + top + radius,
                    right + radius,
                    (bounds.height - top - bottom - 2. * radius).max(0.),
                ),
            ];
        }
        Style::CornerWrap => {
            let h = parameters.height.min(bounds.height);
            let w = parameters.wrap_width.min(bounds.width);
            let horizontal = Rect::new(bounds.x, bounds.y, parameters.wrap_size[0].min(bounds.width), h);
            let vertical = Rect::new(bounds.x, bounds.y, w, parameters.wrap_size[1].min(bounds.height));
            add(
                &mut scene,
                grid,
                horizontal,
                AnchoredRadii([0., 0., 14., 0.]).screen_space(Edge::Top),
                true,
                0,
            )?;
            add(&mut scene, grid, vertical, CornerRadii([0., 0., 14., 0.]), true, 0)?;
            // Capture the union once: overlapping translucent captures darken the elbow.
            scene.damage = vec![
                horizontal,
                Rect::new(vertical.x, vertical.y + h, w, (vertical.height - h).max(0.)),
            ];
            let r = parameters
                .fillet
                .min((horizontal.width - w).max(0.))
                .min((vertical.height - h).max(0.));
            if r > 0. {
                let patch = grid.rect(Rect::new(bounds.x + w, bounds.y + h, r, r))?;
                scene.damage.push(patch);
                scene.paint.push(Primitive {
                    bounds: patch,
                    radii: CornerRadii::uniform(0.),
                    operation: 4,
                    blend_zone: 0,
                });
            }
        }
        Style::Morph | Style::WindowHug => {
            let attached = parameters.style == Style::Morph && parameters.attached;
            let radii = parameters
                .radius
                .map_or(CornerRadii::uniform((bounds.height / 2.) as f32), |r| {
                    r.screen_space(parameters.edge)
                })
                .at_edge(parameters.edge, attached);
            add(&mut scene, grid, bounds, radii, true, 0)?;
            if attached {
                let r = grid.snap(parameters.fillet.min(bounds.height));
                if r > 0. {
                    let (y, left, right) = if parameters.edge == Edge::Top {
                        (bounds.y, 2, 3)
                    } else {
                        (bounds.y + bounds.height - r, 5, 4)
                    };
                    for (x, operation) in [(bounds.x - r, left), (bounds.x + bounds.width, right)] {
                        let patch = grid.rect(Rect::new(x, y, r, r))?;
                        scene.paint.push(Primitive {
                            bounds: patch,
                            radii: CornerRadii::uniform(0.),
                            operation,
                            blend_zone: 0,
                        });
                    }
                    // One material capture owns the body and both shoulders. Separate
                    // captures overlap after logical rounding at fractional scales,
                    // painting translucent joins twice as the spring settles.
                    scene.damage =
                        vec![grid.rect(Rect::new(bounds.x - r, bounds.y, bounds.width + 2. * r, bounds.height))?];
                }
            }
        }
        style => {
            for (zone_index, zone) in [ZonePosition::Start, ZonePosition::Center, ZonePosition::End]
                .into_iter()
                .enumerate()
            {
                let occupied: Vec<_> = groups
                    .iter()
                    .filter(|g| g.zone == zone && g.bounds.width > 0. && g.bounds.height > 0.)
                    .collect();
                if occupied.is_empty() {
                    continue;
                }
                if style.connected() {
                    let x = occupied.iter().map(|g| g.bounds.x).fold(f64::INFINITY, f64::min);
                    let end = occupied
                        .iter()
                        .map(|g| g.bounds.x + g.bounds.width)
                        .fold(f64::NEG_INFINITY, f64::max);
                    let (x, end) = if style == Style::Notch {
                        match zone {
                            ZonePosition::Start => (bounds.x, end),
                            ZonePosition::End => (x, bounds.x + bounds.width),
                            ZonePosition::Center => (x, end),
                        }
                    } else {
                        (x, end)
                    };
                    let body = Rect::new(x, bounds.y, end - x, bounds.height);
                    let radii = parameters.radius.map_or_else(
                        || {
                            crate::shape::effective_radii(
                                style,
                                zone,
                                parameters.edge,
                                [body.width as f32, body.height as f32],
                                0,
                                1,
                            )
                        },
                        |r| {
                            Ok(r.screen_space(parameters.edge)
                                .normalized(body.width as f32, body.height as f32))
                        },
                    )?;
                    add(&mut scene, grid, body, radii, true, 0)?;
                    for group in occupied.iter().take(occupied.len() - 1) {
                        let seam = grid.snap(group.bounds.x + group.bounds.width);
                        scene
                            .dividers
                            .push([seam, bounds.y + 8., seam, bounds.y + bounds.height - 8.]);
                    }
                    if style == Style::Notch && zone == ZonePosition::Center {
                        let r = parameters
                            .fillet
                            .min(bounds.height)
                            .min((x - bounds.x).max(0.))
                            .min((bounds.x + bounds.width - end).max(0.));
                        if r > 0. {
                            let (y, left, right) = if parameters.edge == Edge::Top {
                                (bounds.y, 2, 3)
                            } else {
                                (bounds.y + bounds.height - r, 5, 4)
                            };
                            for (x, operation) in [(x - r, left), (end, right)] {
                                scene.paint.push(Primitive {
                                    bounds: grid.rect(Rect::new(x, y, r, r))?,
                                    radii: CornerRadii::uniform(0.),
                                    operation,
                                    blend_zone: 0,
                                });
                                scene.damage.push(Rect::new(x, y, r, r));
                            }
                        }
                    }
                } else {
                    for group in occupied {
                        let body = Rect::new(group.bounds.x, bounds.y, group.bounds.width, bounds.height);
                        let radii = parameters
                            .radius
                            .map_or(CornerRadii::uniform((bounds.height / 2.) as f32), |r| {
                                r.screen_space(parameters.edge)
                            });
                        add(
                            &mut scene,
                            grid,
                            body,
                            radii,
                            true,
                            if style == Style::Fused {
                                (zone_index + 1) as u8
                            } else {
                                0
                            },
                        )?;
                    }
                }
            }
            // Polynomial smooth-min only bridges when k > 2*gap. A strength
            // of 0.8*gap cannot form a neck with this kernel.
            if style == Style::Fused {
                scene.fusion = parameters.gap * 2.4;
                scene.damage.clear();
                for zone in 1..=3 {
                    let mut shapes = scene.paint.iter().filter(|p| p.blend_zone == zone);
                    if let Some(first) = shapes.next() {
                        let mut region = first.bounds;
                        for shape in shapes {
                            let right = (region.x + region.width).max(shape.bounds.x + shape.bounds.width);
                            region.x = region.x.min(shape.bounds.x);
                            region.width = right - region.x;
                        }
                        scene.damage.push(region);
                    }
                }
            }
        }
    }
    if parameters.edge == Edge::Bottom && matches!(parameters.style, Style::Frame | Style::CornerWrap) {
        let mirror = |mut r: Rect| {
            r.y = 2. * bounds.y + bounds.height - r.y - r.height;
            r
        };
        for primitive in &mut scene.paint {
            primitive.bounds = mirror(primitive.bounds);
            let [tl, tr, br, bl] = primitive.radii.0;
            primitive.radii = CornerRadii([bl, br, tr, tl]);
            primitive.operation = match primitive.operation {
                2 => 5,
                3 => 4,
                4 => 3,
                5 => 2,
                op => op,
            };
        }
        for input in &mut scene.input {
            let [tl, tr, br, bl] = input.radii().0;
            *input = RoundedSurface::new(mirror(input.bounds()), CornerRadii([bl, br, tr, tl]), grid)?;
        }
        for damage in &mut scene.damage {
            *damage = mirror(*damage);
        }
    }
    if scene.paint.len() > MAX_PRIMITIVES {
        return Err("panel shape exceeds renderer primitive capacity");
    }
    scene.damage.retain(|r| r.width > 0. && r.height > 0.);
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn groups() -> [GroupBounds; 3] {
        [
            GroupBounds {
                zone: ZonePosition::Start,
                bounds: Rect::new(8., 0., 100., 36.),
            },
            GroupBounds {
                zone: ZonePosition::Start,
                bounds: Rect::new(116., 0., 80., 36.),
            },
            GroupBounds {
                zone: ZonePosition::Center,
                bounds: Rect::new(350., 0., 100., 36.),
            },
        ]
    }
    fn scene(style: Style) -> Scene {
        derive(
            Parameters {
                style,
                ..Default::default()
            },
            Rect::new(0., 0., 800., 36.),
            &groups(),
            PixelGrid::new(1.25).unwrap(),
        )
        .unwrap()
    }
    #[test]
    fn floating_gaps_are_unpainted_and_click_through() {
        let scene = scene(Style::Floating);
        assert_eq!(scene.paint.len(), 3);
        assert!(scene.distance([112., 18.]) > 0. && !scene.contains([112., 18.]));
        assert!(scene.contains([50., 18.]));
    }
    #[test]
    fn capsules_use_one_outline_per_occupied_zone() {
        let scene = scene(Style::Capsules);
        assert_eq!(scene.paint.len(), 2);
        assert_eq!(scene.dividers.len(), 1);
        assert!(scene.distance([112., 18.]) < 0.);
    }
    #[test]
    fn notch_fillets_are_painted_but_never_interactive() {
        let scene = scene(Style::Notch);
        assert!(scene.distance([349., 1.]) < 0.);
        assert!(!scene.contains([349., 1.]));
        assert!(scene.distance([340.5, 9.5]) > 0.);
        assert_eq!(scene.paint[0].bounds.x, 0.);
    }
    #[test]
    fn fused_neighbors_grow_a_neck_without_capturing_the_gap() {
        let scene = scene(Style::Fused);
        assert!(scene.distance([112., 18.]) < 0.);
        assert!(!scene.contains([112., 18.]));
        assert!(scene.distance([250., 18.]) > 0.);
    }
    #[test]
    fn fused_capture_includes_the_neck() {
        let scene = scene(Style::Fused);
        assert!(
            scene
                .damage
                .iter()
                .any(|r| r.x <= 112. && r.x + r.width > 112. && r.y <= 18. && r.y + r.height > 18.)
        );
    }

    #[test]
    fn corner_wrap_capture_is_disjoint_and_the_workspace_arm_is_interactive() {
        let scene = derive(
            Parameters {
                style: Style::CornerWrap,
                ..Default::default()
            },
            Rect::new(0., 0., 800., 600.),
            &[],
            PixelGrid::new(1.25).unwrap(),
        )
        .unwrap();
        assert!(scene.contains([18., 200.]));
        assert!(!scene.contains([38., 38.])); // the decorative elbow is not input
        for (i, a) in scene.damage.iter().enumerate() {
            for b in &scene.damage[i + 1..] {
                assert!(a.x + a.width <= b.x || b.x + b.width <= a.x || a.y + a.height <= b.y || b.y + b.height <= a.y);
            }
        }
        for x in (0..800).step_by(3) {
            for y in (0..600).step_by(3) {
                let point = [x as f64 + 0.5, y as f64 + 0.5];
                if scene.distance(point) < 0. {
                    assert!(scene.damage.iter().any(|r| point[0] >= r.x
                        && point[0] < r.x + r.width
                        && point[1] >= r.y
                        && point[1] < r.y + r.height));
                }
            }
        }
    }

    #[test]
    fn frame_has_a_cutout_and_bounded_damage() {
        let scene = derive(
            Parameters {
                style: Style::Frame,
                ..Default::default()
            },
            Rect::new(0., 0., 1600., 1000.),
            &[],
            PixelGrid::new(1.5).unwrap(),
        )
        .unwrap();
        assert!(scene.distance([800., 20.]) < 0. && scene.distance([800., 500.]) > 0.);
        assert!(scene.contains([800., 20.]) && !scene.contains([2., 500.]));
        assert!(scene.damage.iter().map(|r| r.width * r.height).sum::<f64>() < 1600. * 1000. / 5.);
    }
    #[test]
    fn primitive_packets_reject_malformed_geometry() {
        let mut packet = scene(Style::Floating).packets()[0];
        for (index, value) in [(0, f32::NAN), (2, 0.), (4, -1.), (8, 2.5), (8, 6.), (9, 4.)] {
            let previous = packet[index];
            packet[index] = value;
            assert!(Primitive::from_packet(packet).is_err());
            packet[index] = previous;
        }
    }
    #[test]
    fn physical_input_runs_exclude_corners_and_coalesce() {
        let scene = scene(Style::Floating);
        let rects = scene.input_rectangles(1.25);
        assert!(rects.len() < 3 * 45);
        assert!(
            !rects
                .iter()
                .any(|r| r[0] <= 140 && 140 < r[0] + r[2] && r[1] <= 22 && 22 < r[1] + r[3])
        );
    }
    #[test]
    fn attached_styles_mirror_the_whole_outline_at_the_bottom() {
        for style in [Style::Notch, Style::Frame, Style::CornerWrap] {
            let bounds = Rect::new(0., 0., 800., if style == Style::Notch { 36. } else { 600. });
            let grid = PixelGrid::new(1.).unwrap();
            let top = derive(
                Parameters {
                    style,
                    ..Default::default()
                },
                bounds,
                &groups(),
                grid,
            )
            .unwrap();
            let bottom = derive(
                Parameters {
                    style,
                    edge: Edge::Bottom,
                    ..Default::default()
                },
                bounds,
                &groups(),
                grid,
            )
            .unwrap();
            for x in (0..800).step_by(13) {
                for y in (0..bounds.height as usize).step_by(11) {
                    let a = top.distance([x as f64 + 0.5, y as f64 + 0.5]);
                    let b = bottom.distance([x as f64 + 0.5, bounds.height - y as f64 - 0.5]);
                    assert!((a - b).abs() < 1e-6, "{style:?}: {x},{y}: {a} != {b}");
                }
            }
        }
    }
}
