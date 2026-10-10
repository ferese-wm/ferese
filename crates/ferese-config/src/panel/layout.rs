//! Measured panel allocation. This module has no frame timing or service ownership.
use super::{ItemId, OverflowPolicy, Panel, Representation, Zone};
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct Measurement {
    pub id: ItemId,
    pub alternatives: Vec<(Representation, f32)>,
    pub minimum: Option<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Placement {
    Visible { representation: Representation, width: f32 },
    Overflow,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resolution {
    pub items: BTreeMap<ItemId, Placement>,
    pub overflow: Vec<ItemId>,
    pub zone_widths: [f32; 3],
    pub forced_overflow: bool,
    pub overflow_trigger_width: f32,
    /// Width needed by the smallest visible representations before automatic overflow.
    pub minimum_width: f32,
}

fn zone_width(background: crate::BarLayout, zone: &Zone, items: &BTreeMap<ItemId, Placement>) -> f32 {
    let mut groups = Vec::new();
    for group in &zone.groups {
        let mut widths = Vec::new();
        for item in &group.items {
            if let Some(Placement::Visible { width, .. }) = items.get(&item.id)
                && *width > 0.0
            {
                if !widths.is_empty() {
                    widths.push(item.gap_before.unwrap_or(group.spacing));
                }
                widths.push(*width);
            }
        }
        if !widths.is_empty() {
            let padding = 2.0 * f32::from(group.padding[1]);
            let island = if background == crate::BarLayout::Islands {
                2.0 * group.island_padding
            } else {
                0.0
            };
            groups.push(widths.iter().sum::<f32>() + padding + island);
        }
    }
    groups.iter().sum::<f32>() + zone.spacing * groups.len().saturating_sub(1) as f32
}

impl Resolution {
    fn update_widths(&mut self, panel: &Panel, overflow_width: f32) {
        self.zone_widths = [
            zone_width(panel.background, &panel.start, &self.items),
            zone_width(panel.background, &panel.center, &self.items),
            zone_width(panel.background, &panel.end, &self.items),
        ];
        if !self.overflow.is_empty() {
            self.zone_widths[2] += overflow_width
                + if self.zone_widths[2] > 0.0 {
                    panel.end.spacing
                } else {
                    0.0
                };
        }
    }

    fn required_width(&self) -> f32 {
        self.required_width_with_gap(8.)
    }

    fn required_width_with_gap(&self, gap: f32) -> f32 {
        let [start, center, end] = self.zone_widths;
        if center > 0.0 {
            2.0 * start.max(end) + center + 2.0 * gap
        } else {
            start + end + if start > 0.0 && end > 0.0 { gap } else { 0.0 }
        }
    }

    pub fn fits(&self, available: f32) -> bool {
        self.required_width() <= available
    }

    /// Clamp a requested inset independently for each output. Content may still
    /// overflow on an output too small to fit it even without an inset.
    pub fn side_margin(&self, requested: i32, output_width: i32, padding: f32) -> i32 {
        let maximum = ((output_width as f32 - 2. * padding - self.minimum_width) / 2.)
            .floor()
            .max(0.);
        requested.max(0).min(maximum as i32)
    }
}

pub fn resolve(panel: &Panel, measurements: &[Measurement], available: f32, overflow_width: f32) -> Resolution {
    resolve_with_gap(panel, measurements, available, overflow_width, 8.)
}

/// Reserve separation between zones while keeping the center on the panel midpoint.
pub fn resolve_with_gap(
    panel: &Panel,
    measurements: &[Measurement],
    available: f32,
    overflow_width: f32,
    zone_gap: f32,
) -> Resolution {
    let available = available.max(0.0);
    let zone_gap = zone_gap.max(0.);
    let fits = |resolution: &Resolution| resolution.required_width_with_gap(zone_gap) <= available;
    let mut result = Resolution::default();
    let ordered: Vec<_> = [&panel.start, &panel.center, &panel.end]
        .into_iter()
        .flat_map(|zone| &zone.groups)
        .flat_map(|group| &group.items)
        .collect();
    let measurements: BTreeMap<_, _> = measurements.iter().map(|measured| (&measured.id, measured)).collect();
    // Allocation policy follows composition, never the renderer's sample order.
    // Lower priorities yield first; definition order breaks equal-priority ties.
    let mut candidates: Vec<_> = ordered.iter().enumerate().collect();
    candidates.sort_by_key(|(index, item)| (item.priority, *index));
    for item in &ordered {
        let Some(measured) = measurements.get(&item.id) else {
            continue;
        };
        let Some(&(representation, width)) = measured.alternatives.first() else {
            continue;
        };
        if item.overflow == OverflowPolicy::Always {
            result.items.insert(item.id.clone(), Placement::Overflow);
            result.overflow.push(item.id.clone());
        } else {
            result
                .items
                .insert(item.id.clone(), Placement::Visible { representation, width });
        }
    }
    result.update_widths(panel, overflow_width);
    let mut minimum = result.clone();
    for measured in measurements.values() {
        if let Some(Placement::Visible { width, .. }) = minimum.items.get_mut(&measured.id) {
            for (_, alternative) in &measured.alternatives {
                *width = width.min(*alternative);
            }
            if let Some(flexible) = measured.minimum {
                *width = width.min(flexible);
            }
        }
    }
    minimum.update_widths(panel, overflow_width);
    result.minimum_width = minimum.required_width_with_gap(zone_gap);
    // Flexible content yields space without changing the stored item preference.
    for (_, item) in &candidates {
        let Some(measured) = measurements.get(&item.id).filter(|measured| measured.minimum.is_some()) else {
            continue;
        };
        if fits(&result) {
            break;
        }
        let Some(Placement::Visible { representation, width }) = result.items.get(&measured.id).cloned() else {
            continue;
        };
        let minimum = measured.minimum.unwrap().min(width);
        result.items.insert(
            measured.id.clone(),
            Placement::Visible {
                representation,
                width: minimum,
            },
        );
        result.update_widths(panel, overflow_width);
    }
    for (_, item) in &candidates {
        if fits(&result) {
            break;
        }
        let Some(measured) = measurements.get(&item.id) else {
            continue;
        };
        for &(representation, width) in measured.alternatives.iter().skip(1) {
            if let Some(Placement::Visible { width: previous, .. }) = result.items.get(&item.id)
                && width < *previous
            {
                result
                    .items
                    .insert(item.id.clone(), Placement::Visible { representation, width });
                result.update_widths(panel, overflow_width);
                if fits(&result) {
                    break;
                }
            }
        }
    }
    // Never is honored whenever the mandatory minimums fit. On physically smaller
    // surfaces, retain access through overflow instead of silently clipping actions.
    for force in [false, true] {
        for (_, item) in &candidates {
            if fits(&result) {
                break;
            }
            if item.overflow == OverflowPolicy::Never && !force {
                continue;
            }
            if matches!(result.items.get(&item.id), Some(Placement::Visible { width, .. }) if *width > 0.0) {
                result.items.insert(item.id.clone(), Placement::Overflow);
                result.overflow.push(item.id.clone());
                result.forced_overflow |= force && item.overflow == OverflowPolicy::Never;
                result.update_widths(panel, overflow_width);
            }
        }
    }
    // Restore the most protected flexible items first, reversing the yield order.
    for (_, item) in candidates.iter().rev() {
        let Some(measured) = measurements.get(&item.id).filter(|measured| measured.minimum.is_some()) else {
            continue;
        };
        let Some(Placement::Visible {
            representation,
            width: minimum,
        }) = result.items.get(&measured.id).cloned()
        else {
            continue;
        };
        let maximum = measured.alternatives[0].1;
        let mut low = minimum;
        let mut high = maximum;
        for _ in 0..20 {
            let width = (low + high) * 0.5;
            result
                .items
                .insert(measured.id.clone(), Placement::Visible { representation, width });
            result.update_widths(panel, overflow_width);
            if fits(&result) { low = width } else { high = width }
        }
        result.items.insert(
            measured.id.clone(),
            Placement::Visible {
                representation,
                width: low,
            },
        );
    }
    result
        .overflow
        .sort_by_key(|id| ordered.iter().position(|item| &item.id == id).unwrap());
    result.update_widths(panel, overflow_width);
    result
}

#[cfg(test)]
mod tests {
    use super::super::Defaults as StatusConfig;
    use super::*;

    #[test]
    fn allocation_is_independent_of_measurement_order_even_for_flexible_items() {
        let panel = Panel::from_defaults(&StatusConfig::default());
        let mut measured = measurements(&panel);
        for measurement in &mut measured {
            measurement.minimum = Some(24.);
        }
        for width in [2400., 1400., 1000., 800., 400., 80., 32.] {
            let expected = resolve_with_gap(&panel, &measured, width, 32., 16.);
            for _ in 0..measured.len() {
                measured.rotate_left(1);
                assert_eq!(resolve_with_gap(&panel, &measured, width, 32., 16.), expected);
                measured.reverse();
                assert_eq!(resolve_with_gap(&panel, &measured, width, 32., 16.), expected);
                measured.reverse();
            }
        }
    }

    fn measurements(panel: &Panel) -> Vec<Measurement> {
        [&panel.start, &panel.center, &panel.end]
            .into_iter()
            .flat_map(|zone| &zone.groups)
            .flat_map(|group| &group.items)
            .map(|item| Measurement {
                id: item.id.clone(),
                alternatives: vec![
                    (Representation::Wide, 80.0),
                    (Representation::Compact, 40.0),
                    (Representation::Icon, 24.0),
                ],
                minimum: matches!(item.kind, crate::panel::ItemKind::FocusedWindow).then_some(48.0),
            })
            .collect()
    }

    #[test]
    fn narrow_panels_adapt_deterministically_and_keep_every_item_reachable() {
        for background in [crate::BarLayout::Continuous, crate::BarLayout::Islands] {
            let panel = Panel::from_defaults(&StatusConfig {
                bar_layout: background,
                ..Default::default()
            });
            let measured = measurements(&panel);
            for width in [2400., 1200., 800., 400., 160., 80., 32.] {
                let resolution = resolve(&panel, &measured, width, 32.);
                assert!(resolution.fits(width));
                assert_eq!(resolution, resolve(&panel, &measured, width, 32.));
                assert_eq!(resolution.items.len(), measured.len());
                for id in &resolution.overflow {
                    assert_eq!(resolution.items.get(id), Some(&Placement::Overflow));
                }
                if width >= 400. {
                    assert!(!resolution.forced_overflow);
                }
            }
        }
    }

    #[test]
    fn decoration_and_borders_do_not_change_background_arrangement_or_allocation() {
        for background in [crate::BarLayout::Continuous, crate::BarLayout::Islands] {
            let mut panel = Panel::from_defaults(&super::super::Defaults {
                bar_layout: background,
                ..Default::default()
            });
            let measurements = measurements(&panel);
            let plain = resolve(&panel, &measurements, 1000., 32.);
            assert!(!panel.border);
            panel.border = true;
            panel.background_opacity = Some(0.4);
            assert_eq!(resolve(&panel, &measurements, 1000., 32.), plain);
            assert_eq!(panel.background, background);
        }
    }

    #[test]
    fn side_margins_stop_before_overflow_and_use_each_outputs_logical_width() {
        let panel = Panel::from_defaults(&super::super::Defaults::default());
        let measurements = measurements(&panel);
        let full = resolve(&panel, &measurements, 4000., 32.);
        assert!(full.minimum_width < full.required_width());
        for output_width in [1600, 2400, 3840] {
            for requested in [0, 32, 400, 4096] {
                let margin = full.side_margin(requested, output_width, 12.);
                let available = output_width as f32 - 2. * (margin as f32 + 12.);
                let result = resolve(&panel, &measurements, available, 32.);
                assert!(result.overflow.is_empty(), "output {output_width}, margin {margin}");
                assert!(result.fits(available));
                assert!(margin <= requested);
            }
        }
        assert!(full.side_margin(4096, 3840, 12.) > full.side_margin(4096, 1600, 12.));
        assert_eq!(full.side_margin(4096, 100, 12.), 0);
        assert!(
            !resolve(&panel, &measurements, 76., 32.).overflow.is_empty(),
            "physical output shortage still uses existing overflow"
        );
    }

    #[test]
    fn margin_limit_includes_group_padding_and_authored_overflow_without_dependence_on_current_surface_width() {
        let mut panel = Panel::from_defaults(&super::super::Defaults::default());
        panel.end.groups[0].items[0].overflow = OverflowPolicy::Always;
        let measurements = measurements(&panel);
        let full = resolve(&panel, &measurements, 4000., 32.);
        let narrow = resolve(&panel, &measurements, 100., 32.);
        assert_eq!(full.minimum_width, narrow.minimum_width);
        let margin = full.side_margin(4096, 2400, 12.);
        let result = resolve(&panel, &measurements, 2400. - 2. * (margin as f32 + 12.), 32.);
        assert_eq!(result.overflow, full.overflow);
        let original = full.minimum_width;
        panel.end.groups[0].padding[1] += 10;
        let padded = resolve(&panel, &measurements, 4000., 32.);
        assert!(padded.minimum_width > original);
        assert!(padded.side_margin(4096, 2400, 12.) < margin);
    }

    #[test]
    fn asymmetric_sides_reserve_a_real_screen_center() {
        let panel = Panel::from_defaults(&StatusConfig::default());
        let resolution = resolve(&panel, &measurements(&panel), 1500., 32.);
        let [start, center, end] = resolution.zone_widths;
        assert!(end > start);
        assert!(start + 8. <= (1500. - center) * 0.5);
        assert!((1500. + center) * 0.5 + 8. <= 1500. - end);
    }

    #[test]
    fn always_overflow_is_reserved_and_unavailable_items_are_not_allocated() {
        let mut panel = Panel::from_defaults(&StatusConfig::default());
        panel.end.groups[0].items[2].overflow = OverflowPolicy::Always;
        let mut measured = measurements(&panel);
        measured.remove(3);
        let resolution = resolve(&panel, &measured, 2000., 32.);
        assert!(!resolution.items.contains_key(&ItemId("media".into())));
        assert_eq!(resolution.overflow, [ItemId("network".into())]);
    }
    #[test]
    fn priority_controls_displacement_and_expansion_restores_requested_sizes() {
        let mut panel = Panel::from_defaults(&StatusConfig::default());
        panel.start.groups.clear();
        panel.center.groups.clear();
        panel.end.groups.retain(|group| group.id.0 == "time");
        let mut first = panel.end.groups[0].items[0].clone();
        first.id = ItemId("low".into());
        first.priority = 0;
        let mut second = first.clone();
        second.id = ItemId("high".into());
        second.priority = 100;
        let mut third = first.clone();
        third.id = ItemId("middle".into());
        third.priority = 50;
        panel.end.groups[0].items = vec![first, second, third];
        let measured: Vec<_> = panel.end.groups[0]
            .items
            .iter()
            .map(|item| Measurement {
                id: item.id.clone(),
                alternatives: vec![(Representation::Wide, 50.)],
                minimum: None,
            })
            .collect();
        let initial = panel.clone();
        let small = resolve(&panel, &measured, 150., 32.);
        assert_eq!(small.overflow, [ItemId("low".into())]);
        assert!(small.fits(150.));
        for width in [200., 150., 40., 200.] {
            let result = resolve(&panel, &measured, width, 32.);
            assert!(result.fits(width));
            if width == 200. {
                assert!(result.overflow.is_empty());
                assert!(result.items.values().all(|placement| matches!(placement, Placement::Visible { representation: Representation::Wide, width } if *width == 50.)));
            }
            assert_eq!(panel, initial, "resolution must not rewrite configuration intent");
        }
    }
}
