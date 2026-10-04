use crate::{Gradient, Paint, PaintStyle, ResolvedTheme};

pub fn rgba(s: &str) -> Result<[f64; 4], String> {
    let hex = s.strip_prefix('#').ok_or_else(|| format!("Invalid color: {s}"))?;
    if !hex.is_ascii() || !matches!(hex.len(), 6 | 8) {
        return Err(format!("Invalid color: {s}"));
    }
    let mut out = [1.; 4];
    for (i, c) in out.iter_mut().enumerate().take(hex.len() / 2) {
        *c = f64::from(u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|_| format!("Invalid color: {s}"))?)
            / 255.;
    }
    Ok(out)
}

pub fn hex(c: [f64; 4]) -> String {
    format!(
        "#{:02X}{:02X}{:02X}{:02X}",
        (c[0].clamp(0., 1.) * 255.).round() as u8,
        (c[1].clamp(0., 1.) * 255.).round() as u8,
        (c[2].clamp(0., 1.) * 255.).round() as u8,
        (c[3].clamp(0., 1.) * 255.).round() as u8
    )
}

pub fn luminance(c: [f64; 4]) -> f64 {
    let linear = |v: f64| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(c[0]) + 0.7152 * linear(c[1]) + 0.0722 * linear(c[2])
}

pub fn contrast(a: [f64; 4], b: [f64; 4]) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

pub fn composite(fg: [f64; 4], bg: [f64; 4]) -> [f64; 4] {
    let mut c = [0.; 4];
    for i in 0..3 {
        c[i] = fg[i] * fg[3] + bg[i] * (1. - fg[3]);
    }
    c[3] = 1.;
    c
}

pub fn blend(a: [f64; 4], b: [f64; 4], p: f64) -> [f64; 4] {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * p)
}

pub fn readable(preferred: [f64; 4], background: [f64; 4], minimum: f64) -> [f64; 4] {
    let mut preferred = preferred;
    preferred[3] = 1.;
    if contrast(preferred, background) >= minimum {
        return preferred;
    }
    let black = [0., 0., 0., 1.];
    let white = [1., 1., 1., 1.];
    let target = if contrast(black, background) > contrast(white, background) {
        black
    } else {
        white
    };
    for step in 1..=100 {
        let c = blend(preferred, target, f64::from(step) / 100.);
        if contrast(rgba(&hex(c)).expect("generated color"), background) >= minimum {
            return c;
        }
    }
    target
}

pub fn readable_across(mut preferred: [f64; 4], backgrounds: &[[f64; 4]], minimum: f64) -> [f64; 4] {
    preferred[3] = 1.;
    let score = |color| {
        backgrounds
            .iter()
            .map(|background| contrast(color, *background))
            .fold(f64::INFINITY, f64::min)
    };
    if score(preferred) >= minimum {
        return preferred;
    }
    let black = [0., 0., 0., 1.];
    let white = [1.; 4];
    let target = if score(black) > score(white) { black } else { white };
    for step in 1..=100 {
        let color = rgba(&hex(blend(preferred, target, f64::from(step) / 100.))).unwrap();
        if score(color) >= minimum {
            return color;
        }
    }
    target
}

pub fn material_bounds(mut surface: [f64; 4], opacity: f64) -> [[f64; 4]; 2] {
    surface[3] = opacity;
    [composite(surface, [0., 0., 0., 1.]), composite(surface, [1.; 4])]
}

fn material_foreground(preferred: [f64; 4], backgrounds: [[f64; 4]; 2]) -> [f64; 4] {
    let black = [0., 0., 0., 1.];
    let white = [1.; 4];
    let (target, worst) = if contrast(black, backgrounds[0]) >= contrast(white, backgrounds[1]) {
        (black, backgrounds[0])
    } else {
        (white, backgrounds[1])
    };
    let result = readable(preferred, worst, 4.5);
    let outside = luminance(result) < luminance(backgrounds[0]) || luminance(result) > luminance(backgrounds[1]);
    if outside
        && backgrounds
            .iter()
            .all(|background| contrast(rgba(&hex(result)).unwrap(), *background) >= 4.5)
    {
        result
    } else {
        target
    }
}

fn transition_paint(from: &Paint, to: &Paint, from_solid: &str, to_solid: &str, p: f64) -> Paint {
    if from.effective_gradient().is_none() && to.effective_gradient().is_none() {
        return to.clone();
    }

    let endpoints = |paint: &Paint, solid: &str| {
        paint.effective_gradient().map_or_else(
            || (solid.to_owned(), solid.to_owned()),
            |gradient| (gradient.from.clone(), gradient.to.clone()),
        )
    };
    let (a, b) = endpoints(from, from_solid);
    let (c, d) = endpoints(to, to_solid);
    let color = |a: &str, b: &str| {
        let mut a = rgba(a).unwrap();
        let mut b = rgba(b).unwrap();
        for channel in 0..3 {
            a[channel] *= a[3];
            b[channel] *= b[3];
        }

        let mut c = blend(a, b, p);
        if c[3] > 0. {
            for channel in 0..3 {
                c[channel] /= c[3];
            }
        }

        hex(c)
    };
    let from_angle = from
        .effective_gradient()
        .or(to.effective_gradient())
        .unwrap()
        .angle
        .rem_euclid(360.);
    let to_angle = to
        .effective_gradient()
        .or(from.effective_gradient())
        .unwrap()
        .angle
        .rem_euclid(360.);
    let delta = (to_angle - from_angle + 180.).rem_euclid(360.) - 180.;
    Paint {
        style: PaintStyle::Auto,
        gradient: Some(Gradient {
            from: color(&a, &c),
            to: color(&b, &d),
            angle: (from_angle + delta * p).rem_euclid(360.),
        }),
    }
}

impl ResolvedTheme {
    /// Bound contrast across all backdrops instead of interpolating foregrounds.
    pub fn transition(&self, to: &Self, progress: f64) -> Self {
        if to.reduced_motion || self.accessibility != to.accessibility || progress >= 1.0 {
            return to.clone();
        }
        if progress <= 0.0 || self == to {
            return self.clone();
        }
        let p = progress.clamp(0., 1.);
        let p = p * p * (3. - 2. * p);
        let mut frame = to.clone();
        let mix = |a: &str, b: &str| hex(blend(rgba(a).unwrap(), rgba(b).unwrap(), p));
        frame.tokens.colors.surface_base = mix(&self.tokens.colors.surface_base, &to.tokens.colors.surface_base);
        frame.tokens.colors.surface_raised = mix(&self.tokens.colors.surface_raised, &to.tokens.colors.surface_raised);
        frame.tokens.colors.application_background = mix(
            &self.tokens.colors.application_background,
            &to.tokens.colors.application_background,
        );
        frame.tokens.colors.accent = mix(&self.tokens.colors.accent, &to.tokens.colors.accent);
        frame.tokens.colors.border = mix(&self.tokens.colors.border, &to.tokens.colors.border);
        frame.tokens.colors.shadow = mix(&self.tokens.colors.shadow, &to.tokens.colors.shadow);
        frame.tokens.focus_ring = transition_paint(
            &self.tokens.focus_ring,
            &to.tokens.focus_ring,
            &self.tokens.colors.accent,
            &to.tokens.colors.accent,
            p,
        );
        frame.tokens.border = transition_paint(
            &self.tokens.border,
            &to.tokens.border,
            &self.tokens.colors.border,
            &to.tokens.colors.border,
            p,
        );
        frame.tokens.surface.bar.background =
            mix(&self.tokens.surface.bar.background, &to.tokens.surface.bar.background);
        frame.tokens.material.opacity =
            self.tokens.material.opacity + (to.tokens.material.opacity - self.tokens.material.opacity) * p;
        frame.tokens.material.tint_strength = self.tokens.material.tint_strength
            + (to.tokens.material.tint_strength - self.tokens.material.tint_strength) * p;
        frame.tokens.shadow.soft.opacity =
            self.tokens.shadow.soft.opacity + (to.tokens.shadow.soft.opacity - self.tokens.shadow.soft.opacity) * p;
        let preferred = if p < 0.5 { self } else { to };
        frame.appearance = preferred.appearance;
        if self.appearance == to.appearance {
            let body = [
                rgba(&frame.tokens.colors.surface_base).unwrap(),
                rgba(&frame.tokens.colors.surface_raised).unwrap(),
                rgba(&frame.tokens.colors.application_background).unwrap(),
            ];
            let minimum = if to.accessibility.increase_contrast { 7.0 } else { 4.5 };
            frame.tokens.colors.text_primary = hex(readable_across(
                rgba(&preferred.tokens.colors.text_primary).unwrap(),
                &body,
                minimum,
            ));
            frame.tokens.colors.text_muted = hex(readable_across(
                rgba(&preferred.tokens.colors.text_muted).unwrap(),
                &body,
                minimum,
            ));
            let bar = rgba(&frame.tokens.surface.bar.background).unwrap();
            frame.tokens.surface.bar.text_primary = hex(readable(
                rgba(&preferred.tokens.surface.bar.text_primary).unwrap(),
                bar,
                minimum,
            ));
            frame.tokens.surface.bar.text_muted = hex(readable(
                rgba(&preferred.tokens.surface.bar.text_muted).unwrap(),
                bar,
                minimum,
            ));
            frame.tokens.colors.on_accent = hex(readable(
                rgba(&preferred.tokens.colors.on_accent).unwrap(),
                rgba(&frame.tokens.colors.accent).unwrap(),
                4.5,
            ));
            return frame;
        }
        let base = rgba(&frame.tokens.colors.surface_base).unwrap();
        let raised = rgba(&frame.tokens.colors.surface_raised).unwrap();
        let app = rgba(&frame.tokens.colors.application_background).unwrap();
        let minimum = [base, raised, app]
            .into_iter()
            .map(luminance)
            .fold(f64::INFINITY, f64::min);
        let maximum = [base, raised, app].into_iter().map(luminance).fold(0., f64::max);
        if ((minimum + 0.05) / 0.05).max(1.05 / (maximum + 0.05)) < 4.5 {
            frame.tokens.colors.surface_raised = frame.tokens.colors.surface_base.clone();
            frame.tokens.colors.application_background = frame.tokens.colors.surface_base.clone();
        }
        let surface = rgba(&frame.tokens.colors.surface_base).unwrap();
        let bar = rgba(&frame.tokens.surface.bar.background).unwrap();
        let mut opacity = if frame.tokens.material.style == "solid" {
            1.
        } else {
            frame.tokens.material.opacity * frame.tokens.material.tint_strength
        };
        for step in 0..=100 {
            let candidate = opacity + (1. - opacity) * f64::from(step) / 100.;
            let backgrounds = material_bounds(surface, candidate);
            let raised = rgba(&frame.tokens.colors.surface_raised).unwrap();
            let app = rgba(&frame.tokens.colors.application_background).unwrap();
            let all = [backgrounds[0], backgrounds[1], raised, app];
            let minimum = all.into_iter().map(luminance).fold(f64::INFINITY, f64::min);
            let maximum = all.into_iter().map(luminance).fold(0., f64::max);
            let bar = material_bounds(bar, candidate);
            let readable = ((minimum + 0.05) / 0.05).max(1.05 / (maximum + 0.05)) >= 4.5
                && contrast([0., 0., 0., 1.], bar[0]).max(contrast([1.; 4], bar[1])) >= 4.5;
            if readable {
                opacity = candidate;
                break;
            }
        }
        if opacity > frame.tokens.material.opacity {
            frame.tokens.material.opacity = opacity;
            frame.tokens.material.tint_strength = 1.;
        } else if frame.tokens.material.opacity > 0. {
            frame.tokens.material.tint_strength = opacity / frame.tokens.material.opacity;
        }
        let surface = material_bounds(surface, opacity);
        let bar = material_bounds(bar, opacity);
        let raised = rgba(&frame.tokens.colors.surface_raised).unwrap();
        let app = rgba(&frame.tokens.colors.application_background).unwrap();
        let body_backgrounds = [surface[0], surface[1], raised, app];
        let body_bounds = [
            *body_backgrounds
                .iter()
                .min_by(|a, b| luminance(**a).total_cmp(&luminance(**b)))
                .unwrap(),
            *body_backgrounds
                .iter()
                .max_by(|a, b| luminance(**a).total_cmp(&luminance(**b)))
                .unwrap(),
        ];
        frame.tokens.colors.text_primary = hex(material_foreground(
            rgba(&preferred.tokens.colors.text_primary).unwrap(),
            body_bounds,
        ));
        frame.tokens.colors.text_muted = hex(material_foreground(
            rgba(&preferred.tokens.colors.text_muted).unwrap(),
            body_bounds,
        ));
        frame.tokens.surface.bar.text_primary = hex(material_foreground(
            rgba(&preferred.tokens.surface.bar.text_primary).unwrap(),
            bar,
        ));
        frame.tokens.surface.bar.text_muted = hex(material_foreground(
            rgba(&preferred.tokens.surface.bar.text_muted).unwrap(),
            bar,
        ));
        frame.tokens.colors.on_accent = hex(readable(
            rgba(&preferred.tokens.colors.on_accent).unwrap(),
            rgba(&frame.tokens.colors.accent).unwrap(),
            4.5,
        ));
        frame
    }
}
