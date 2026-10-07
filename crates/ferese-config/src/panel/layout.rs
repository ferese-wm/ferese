//! Measured panel allocation. This module has no frame timing or service ownership.
use super::{GroupSurface, ItemId, OverflowPolicy, Panel, Representation, Zone};
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
}

fn zone_width(zone: &Zone, items: &BTreeMap<ItemId, Placement>) -> f32 {
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
            let padding = if group.surface == GroupSurface::None {
                0.0
            } else {
                2.0 * f32::from(group.padding[1])
            };
            let island = if group.surface == GroupSurface::Island {
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
            zone_width(&panel.start, &self.items),
            zone_width(&panel.center, &self.items),
            zone_width(&panel.end, &self.items),
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

    pub fn fits(&self, available: f32) -> bool {
        let [start, center, end] = self.zone_widths;
        if center > 0.0 {
            2.0 * start.max(end) + center + 16.0 <= available
        } else {
            start + end + if start > 0.0 && end > 0.0 { 8.0 } else { 0.0 } <= available
        }
    }
}

pub fn resolve(panel: &Panel, measurements: &[Measurement], available: f32, overflow_width: f32) -> Resolution {
    let available = available.max(0.0);
    let mut result = Resolution::default();
    let ordered: Vec<_> = [&panel.start, &panel.center, &panel.end]
        .into_iter()
        .flat_map(|zone| &zone.groups)
        .flat_map(|group| &group.items)
        .collect();
    for item in &ordered {
        let Some(measured) = measurements.iter().find(|measured| measured.id == item.id) else {
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
    // Flexible content yields space without changing the stored item preference.
    for measured in measurements.iter().filter(|measured| measured.minimum.is_some()) {
        if result.fits(available) {
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
    let mut candidates: Vec<_> = ordered.iter().enumerate().collect();
    candidates.sort_by_key(|(index, item)| (item.priority, *index));
    for (_, item) in &candidates {
        if result.fits(available) {
            break;
        }
        let Some(measured) = measurements.iter().find(|measured| measured.id == item.id) else {
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
                if result.fits(available) {
                    break;
                }
            }
        }
    }
    // Never is honored whenever the mandatory minimums fit. On physically smaller
    // surfaces, retain access through overflow instead of silently clipping actions.
    for force in [false, true] {
        for (_, item) in &candidates {
            if result.fits(available) {
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
    // Restore flexible widths up to their measured preference with the remaining space.
    for measured in measurements.iter().filter(|measured| measured.minimum.is_some()) {
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
            if result.fits(available) {
                low = width
            } else {
                high = width
            }
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
                minimum: matches!(item.kind, crate::panel::ItemKind::FocusedWindow { .. }).then_some(48.0),
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
