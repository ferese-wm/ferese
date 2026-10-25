use super::{CornerRadii, GeometryIntent, PanelGeometry};
use crate::PanelStyle;

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

/// The effects protocol accepts 32 regions; reserve one for the overflow group.
pub const MAX_GROUPS: usize = 31;

fn yes() -> bool {
    true
}
fn priority() -> u8 {
    50
}
fn spacing() -> f32 {
    1.0
}
fn padding() -> [u16; 2] {
    [2, 3]
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct PanelId(pub String);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    #[default]
    Top,
    Bottom,
}

impl Edge {
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Bottom => "bottom",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(transparent)]
pub struct GroupId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(transparent)]
pub struct ItemId(pub String);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Representation {
    Wide,
    Compact,
    Icon,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OverflowPolicy {
    #[default]
    Auto,
    Never,
    Always,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkspaceStyle {
    Numbers,
    #[default]
    Dots,
    Tabs,
    WindowStacks,
    AppIcons,
}

impl WorkspaceStyle {
    pub const ALL: [Self; 5] = [
        Self::Numbers,
        Self::Dots,
        Self::Tabs,
        Self::WindowStacks,
        Self::AppIcons,
    ];

    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Numbers => "numbers",
            Self::Dots => "dots",
            Self::Tabs => "tabs",
            Self::WindowStacks => "window-stacks",
            Self::AppIcons => "app-icons",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Numbers => "Numbers",
            Self::Dots => "Dots",
            Self::Tabs => "Tabs",
            Self::WindowStacks => "Window stacks",
            Self::AppIcons => "App icons",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ItemKind {
    Overview,
    Workspaces {
        #[serde(default)]
        style: WorkspaceStyle,
    },
    FocusedWindow,
    Media,
    Activities,
    QuickSettings,
    NightLight,
    Network,
    Audio,
    Recording,
    CaptureStatus,
    Notifications,
    Battery {
        #[serde(default = "yes")]
        percentage: bool,
    },
    Clock,
    DisplayMode,
    Overflow,
}

impl ItemKind {
    /// Smallest useful allocation before a flexible control moves into overflow.
    #[must_use]
    pub fn minimum_width(self) -> Option<f32> {
        match self {
            Self::FocusedWindow => Some(48.),
            Self::Workspaces { .. } => Some(24.),
            Self::Media => Some(112.),
            _ => None,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Workspaces { .. } => "Workspaces",
            Self::FocusedWindow => "Focused window",
            Self::Media => "Media",
            Self::Activities => "Activities",
            Self::QuickSettings => "Quick Settings",
            Self::NightLight => "Night light",
            Self::Network => "Network",
            Self::Audio => "Audio",
            Self::Recording => "Recording",
            Self::CaptureStatus => "Capture status",
            Self::Notifications => "Notifications",
            Self::Battery { .. } => "Battery",
            Self::Clock => "Clock",
            Self::DisplayMode => "Display mode",
            Self::Overflow => "Overflow",
        }
    }
    #[must_use]
    pub fn representations(self) -> &'static [Representation] {
        use Representation::*;
        match self {
            Self::Activities | Self::Media | Self::Clock | Self::CaptureStatus => {
                &[Wide, Compact, Icon]
            }
            Self::Battery { percentage: true } => &[Wide, Icon],
            Self::Workspaces { .. } | Self::FocusedWindow => &[Wide],
            _ => &[Icon],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Item {
    pub id: ItemId,
    #[serde(flatten)]
    pub kind: ItemKind,
    // Preserve the existing gap between status controls and the clock/display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gap_before: Option<f32>,
    #[serde(default)]
    pub overflow: OverflowPolicy,
    #[serde(default = "priority")]
    pub priority: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub representation: Option<Representation>,
    #[serde(default = "yes")]
    pub visible: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl<'de> Deserialize<'de> for Item {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let fields = value
            .as_object()
            .ok_or_else(|| serde::de::Error::custom("expected a panel item"))?;
        const OPTIONS: &[&str] = &[
            "id",
            "kind",
            "gap_before",
            "overflow",
            "priority",
            "representation",
            "visible",
            "metadata",
            "style",
            "percentage",
        ];
        for key in fields.keys() {
            if !OPTIONS.contains(&key.as_str()) {
                return Err(serde::de::Error::unknown_field(key, OPTIONS));
            }
        }
        if fields.contains_key("style")
            && fields.get("kind").and_then(serde_json::Value::as_str) != Some("workspaces")
        {
            return Err(serde::de::Error::custom(
                "style is only supported by Workspaces items",
            ));
        }
        if fields.contains_key("percentage")
            && fields.get("kind").and_then(serde_json::Value::as_str) != Some("battery")
        {
            return Err(serde::de::Error::custom(
                "percentage is only supported by Battery items",
            ));
        }
        #[derive(Deserialize)]
        struct Fields {
            id: ItemId,
            #[serde(flatten)]
            kind: ItemKind,
            #[serde(default)]
            gap_before: Option<f32>,
            #[serde(default)]
            overflow: OverflowPolicy,
            #[serde(default = "priority")]
            priority: u8,
            #[serde(default)]
            representation: Option<Representation>,
            #[serde(default = "yes")]
            visible: bool,
            #[serde(default)]
            metadata: BTreeMap<String, serde_json::Value>,
        }
        let item: Fields = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        Ok(Self {
            id: item.id,
            kind: item.kind,
            gap_before: item.gap_before,
            overflow: item.overflow,
            priority: item.priority,
            representation: item.representation,
            visible: item.visible,
            metadata: item.metadata,
        })
    }
}

impl Item {
    /// Start at the configured preference and retain supported smaller forms.
    #[must_use]
    pub fn representations(&self) -> Vec<Representation> {
        let supported = self.kind.representations();
        let preferred = self.representation.unwrap_or(supported[0]);
        let alternatives: Vec<_> = supported
            .iter()
            .copied()
            .filter(|representation| *representation as u8 >= preferred as u8)
            .collect();
        if alternatives.is_empty() {
            vec![supported[0]]
        } else {
            alternatives
        }
    }
    #[must_use]
    pub fn new(id: &str, kind: ItemKind) -> Self {
        Self {
            id: ItemId(id.into()),
            kind,
            gap_before: None,
            representation: None,
            visible: true,
            metadata: BTreeMap::new(),
            overflow: if matches!(
                kind,
                ItemKind::Overview | ItemKind::Workspaces { .. } | ItemKind::FocusedWindow
            ) {
                OverflowPolicy::Never
            } else {
                OverflowPolicy::Auto
            },
            priority: if matches!(kind, ItemKind::Overview | ItemKind::Workspaces { .. }) {
                100
            } else if kind == ItemKind::Activities {
                20
            } else {
                50
            },
        }
    }

    #[must_use]
    pub fn available(&self, availability: Availability) -> bool {
        if !self.visible {
            return false;
        }
        match self.kind {
            ItemKind::Activities => availability.activities,
            ItemKind::Media => availability.media,
            ItemKind::CaptureStatus => availability.capture,
            ItemKind::Network => availability.network,
            ItemKind::Audio => availability.audio,
            ItemKind::Notifications => availability.notifications,
            ItemKind::Battery { .. } => availability.battery,
            ItemKind::DisplayMode => availability.external_display,
            _ => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Availability {
    pub capture: bool,
    pub media: bool,
    pub activities: bool,
    pub network: bool,
    pub audio: bool,
    pub notifications: bool,
    pub battery: bool,
    pub external_display: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PanelSurface {
    None,
    #[default]
    Solid,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum GroupSurface {
    #[default]
    None,
    Inset,
    Island,
}

impl GroupSurface {
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Inset => "inset",
            Self::Island => "island",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub id: GroupId,
    #[serde(default)]
    pub items: Vec<Item>,
    #[serde(default = "spacing")]
    pub spacing: f32,
    #[serde(default = "padding")]
    pub padding: [u16; 2],
    #[serde(default = "crate::default_island_padding")]
    pub island_padding: f32,
}

impl Group {
    pub(super) fn new(id: &str, items: Vec<Item>, padding: [u16; 2], island_padding: f32) -> Self {
        Self {
            id: GroupId(id.into()),
            items,
            spacing: 1.0,
            padding,
            island_padding,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Zone {
    pub groups: Vec<Group>,
    pub spacing: f32,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Panel {
    pub id: PanelId,
    #[serde(default)]
    pub edge: Edge,
    #[serde(default)]
    pub geometry: GeometryIntent,
    #[serde(default)]
    pub style: PanelStyle,
    #[serde(default, skip_serializing_if = "default_morph")]
    pub morph: crate::morph::Profile,
    #[serde(default)]
    pub border: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_opacity: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corner_radius: Option<CornerRadii>,
    #[serde(default)]
    pub start: Zone,
    #[serde(default)]
    pub center: Zone,
    #[serde(default)]
    pub end: Zone,
}

impl Default for Zone {
    fn default() -> Self {
        Self {
            groups: Vec::new(),
            spacing: 8.0,
        }
    }
}

fn default_morph(profile: &crate::morph::Profile) -> bool {
    *profile == crate::morph::Profile::default()
}

pub fn validate(panels: &[Panel]) -> Result<(), String> {
    if panels.len() != 1 {
        return Err("configure exactly one panel; multiple panels are not supported yet".into());
    }
    let valid_id = |value: &str| {
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:@".contains(&byte))
            && !value.starts_with('_')
    };
    let mut panel_ids = HashSet::new();
    for panel in panels {
        if !PanelStyle::AVAILABLE.contains(&panel.style) {
            return Err(format!(
                "panel style {} is not available yet",
                panel.style.key()
            ));
        }
        panel.geometry.validate()?;
        panel.morph.validate()?;
        if panel.group_count() > MAX_GROUPS {
            return Err(format!(
                "a panel supports at most {MAX_GROUPS} groups, reserving one region for overflow"
            ));
        }
        if let Some(opacity) = panel.background_opacity
            && (!opacity.is_finite() || !(0.0..=1.0).contains(&opacity))
        {
            return Err("panel background opacity must be between 0 and 1".into());
        }
        if let Some(radius) = panel.corner_radius {
            radius.validate()?;
        }
        if !valid_id(&panel.id.0) || !panel_ids.insert(&panel.id.0) {
            return Err(format!("invalid or duplicate panel ID {:?}", panel.id.0));
        }
        let mut group_ids = HashSet::new();
        let mut item_ids = HashSet::new();
        for zone in [&panel.start, &panel.center, &panel.end] {
            if !zone.spacing.is_finite() || !(0.0..=64.0).contains(&zone.spacing) {
                return Err("panel zone spacing must be between 0 and 64".into());
            }
            for group in &zone.groups {
                if !valid_id(&group.id.0) || !group_ids.insert(&group.id.0) {
                    return Err(format!("invalid or duplicate group ID {:?}", group.id.0));
                }
                if !group.spacing.is_finite()
                    || !(0.0..=64.0).contains(&group.spacing)
                    || !group.island_padding.is_finite()
                    || !(0.0..=32.0).contains(&group.island_padding)
                    || group.padding.iter().any(|value| *value > 32)
                {
                    return Err("invalid panel group spacing or padding".into());
                }
                for item in &group.items {
                    if !valid_id(&item.id.0) || !item_ids.insert(&item.id.0) {
                        return Err(format!("invalid or duplicate item ID {:?}", item.id.0));
                    }
                    if item.priority > 100
                        || item
                            .gap_before
                            .is_some_and(|gap| !gap.is_finite() || !(0.0..=64.0).contains(&gap))
                    {
                        return Err("panel item priority must be 0..100 and gaps 0..64".into());
                    }
                    if item.kind == ItemKind::Overflow {
                        return Err(
                            "overflow is supplied by the panel; it cannot be configured as an item"
                                .into(),
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

impl Default for Panel {
    fn default() -> Self {
        Self::from_preset(PanelStyle::Flat)
    }
}

impl Panel {
    #[must_use]
    pub fn preset(&self) -> Option<PanelStyle> {
        Some(self.style)
    }

    pub fn apply_preset(&mut self, style: PanelStyle) {
        self.style = style;
    }

    #[must_use]
    pub fn surface(&self) -> PanelSurface {
        if matches!(self.style, PanelStyle::Flat | PanelStyle::Frame) {
            PanelSurface::Solid
        } else {
            PanelSurface::None
        }
    }

    #[must_use]
    pub fn group_surface(&self) -> GroupSurface {
        if self.surface() == PanelSurface::Solid {
            GroupSurface::None
        } else {
            GroupSurface::Island
        }
    }

    #[must_use]
    pub fn zone_spacing(&self, zone: &Zone) -> f32 {
        if self.style.connected() {
            0.
        } else {
            zone.spacing
        }
    }

    #[must_use]
    pub fn shape_parameters(&self) -> crate::scene::Parameters {
        crate::scene::Parameters {
            style: self.style,
            attached: self.style == PanelStyle::Morph && self.morph.attached,
            fillet: if self.style == PanelStyle::Morph {
                12.
            } else {
                crate::scene::Parameters::default().fillet
            },
            edge: self.edge,
            height: self.resolved_geometry().height.into(),
            radius: self.corner_radius.map(|r| crate::shape::AnchoredRadii(r.0)),
            ..Default::default()
        }
    }

    #[must_use]
    pub fn resolved_geometry(&self) -> PanelGeometry {
        let mut geometry = self.geometry.resolve(self.style);
        if self.style == PanelStyle::Morph && self.morph.attached {
            geometry.edge_margin = 0;
        }
        geometry
    }

    #[must_use]
    pub fn exclusive_zone(&self, hidden: bool) -> i32 {
        if hidden {
            0
        } else {
            let geometry = self.resolved_geometry();
            (geometry.height.round() as i32).saturating_add(geometry.window_clearance)
        }
    }

    #[must_use]
    pub fn group_count(&self) -> usize {
        [&self.start, &self.center, &self.end]
            .into_iter()
            .map(|zone| zone.groups.len())
            .sum()
    }

    /// Appearance changes do not replace item ownership or measured allocation.
    #[must_use]
    pub fn same_composition(&self, other: &Self) -> bool {
        let same_zone = |left: &Zone, right: &Zone| {
            left.spacing == right.spacing
                && left.groups.len() == right.groups.len()
                && left.groups.iter().zip(&right.groups).all(|(left, right)| {
                    left.id == right.id
                        && left.items == right.items
                        && left.spacing == right.spacing
                        && left.padding == right.padding
                        && left.island_padding == right.island_padding
                        && (self.group_surface() == GroupSurface::Island)
                            == (other.group_surface() == GroupSurface::Island)
                })
        };
        self.id == other.id
            && self.edge == other.edge
            && self.style == other.style
            && self.morph == other.morph
            && same_zone(&self.start, &other.start)
            && same_zone(&self.center, &other.center)
            && same_zone(&self.end, &other.end)
    }

    #[must_use]
    pub fn resolved_radius(&self, shell_radius: f32) -> CornerRadii {
        self.corner_radius
            .unwrap_or_else(|| CornerRadii::uniform(shell_radius))
    }

    /// The overflow trigger is generated, never persisted as a configured item.
    #[must_use]
    pub fn overflow_group(&self) -> Group {
        Group {
            id: GroupId("_overflow".into()),
            items: vec![Item {
                id: ItemId("_overflow".into()),
                kind: ItemKind::Overflow,
                gap_before: None,
                representation: None,
                visible: true,
                metadata: BTreeMap::new(),
                overflow: OverflowPolicy::Never,
                priority: 100,
            }],
            spacing: 0.0,
            padding: [2, 3],
            island_padding: self
                .end
                .groups
                .first()
                .map_or(crate::default_island_padding(), |group| {
                    group.island_padding
                }),
        }
    }

    #[must_use]
    pub fn item(&self, id: &ItemId) -> Option<&Item> {
        [&self.start, &self.center, &self.end]
            .into_iter()
            .flat_map(|zone| &zone.groups)
            .flat_map(|group| &group.items)
            .find(|item| &item.id == id)
    }

    /// Build a stable initial composition; presets are an editor convenience.
    #[must_use]
    pub fn from_preset(preset: PanelStyle) -> Self {
        use ItemKind::*;

        let item = Item::new;
        let padding = crate::default_island_padding();
        let overview = item("overview", Overview);
        let workspaces = item(
            "workspaces",
            Workspaces {
                style: WorkspaceStyle::default(),
            },
        );
        let title = item("focused-window", FocusedWindow);
        let controls = vec![
            item("media", Media),
            item("quick-settings", QuickSettings),
            item("night-light", NightLight),
            item("network", Network),
            item("audio", Audio),
            item("recording", Recording),
            item("capture-status", CaptureStatus),
            item("notifications", Notifications),
            item("battery", Battery { percentage: true }),
        ];
        let clock = item("clock", Clock);
        let display = item("display-mode", DisplayMode);

        let (start, center, end) = match preset {
            PanelStyle::Flat => (
                vec![
                    Group::new("overview", vec![overview], [0, 0], padding),
                    Group::new("workspaces", vec![workspaces], [2, 3], padding),
                ],
                Group::new("title", vec![title], [0, 0], padding),
                vec![
                    Group::new("status", controls, [2, 3], padding),
                    Group::new("time", vec![clock], [2, 3], padding),
                    Group::new("display", vec![display], [0, 0], padding),
                ],
            ),
            _ => {
                let mut controls = controls;
                controls.extend([clock, display]);
                (
                    vec![Group::new(
                        "navigation",
                        vec![overview, workspaces],
                        [2, 3],
                        padding,
                    )],
                    Group::new("title", vec![title], [0, 0], padding),
                    vec![Group::new("status", controls, [2, 3], padding)],
                )
            }
        };

        Self {
            id: PanelId("main".into()),
            edge: Edge::Top,
            geometry: GeometryIntent::default(),
            style: preset,
            morph: Default::default(),
            border: false,
            background_opacity: Option::None,
            corner_radius: Option::None,
            start: Zone {
                groups: start,
                spacing: if preset == PanelStyle::Floating {
                    8.0
                } else {
                    3.0
                },
            },
            center: Zone {
                groups: vec![center],
                spacing: 8.0,
            },
            end: Zone {
                groups: end,
                spacing: 8.0,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullscreen_releases_the_entire_reservation_without_discarding_pins() {
        let mut panel = Panel::from_preset(PanelStyle::Floating);
        panel.geometry.height = Some(36.);
        panel.geometry.window_clearance = Some(4);
        assert_eq!(panel.exclusive_zone(false), 40);
        assert_eq!(panel.exclusive_zone(true), 0);
        assert_eq!(panel.geometry.window_clearance, Some(4));
    }

    fn items(panel: &Panel) -> Vec<&Item> {
        [&panel.start, &panel.center, &panel.end]
            .into_iter()
            .flat_map(|zone| &zone.groups)
            .flat_map(|group| &group.items)
            .collect()
    }

    #[test]
    fn group_capacity_reserves_overflow_even_for_empty_groups() {
        for background in [PanelStyle::Flat, PanelStyle::Floating] {
            let mut panel = Panel::from_preset(background);
            panel.start.groups.clear();
            panel.center.groups.clear();
            panel.end.groups = (0..MAX_GROUPS)
                .map(|i| Group::new(&format!("group-{i}"), Vec::new(), [2, 3], 4.))
                .collect();
            assert_eq!(panel.group_count(), MAX_GROUPS);
            validate(std::slice::from_ref(&panel)).unwrap();
            panel
                .start
                .groups
                .push(Group::new("one-too-many", Vec::new(), [2, 3], 4.));
            assert!(validate(&[panel]).unwrap_err().contains("31 groups"));
        }
    }

    #[test]
    fn defaults_preserve_order_and_ids_across_background_changes() {
        let continuous = Panel::default();
        let islands = Panel::from_preset(PanelStyle::Floating);
        let expected = [
            "overview",
            "workspaces",
            "focused-window",
            "media",
            "quick-settings",
            "night-light",
            "network",
            "audio",
            "recording",
            "capture-status",
            "notifications",
            "battery",
            "clock",
            "display-mode",
        ];
        for panel in [&continuous, &islands] {
            let ids: Vec<_> = items(panel).iter().map(|item| item.id.0.as_str()).collect();
            assert_eq!(ids, expected);
            assert_eq!(Panel::from_preset(panel.preset().unwrap()), *panel);
        }
        assert_eq!(continuous.start.groups.len(), 2);
        assert_eq!(continuous.end.groups.len(), 3);
        assert_eq!(islands.start.groups.len(), 1);
        assert_eq!(islands.end.groups.len(), 1);
        assert!(!islands.border && !continuous.border);
    }

    #[test]
    fn availability_filters_without_removing_or_reidentifying_instances() {
        let panel = Panel::default();
        let all = items(&panel);
        let visible = |availability| {
            all.iter()
                .filter(|item| item.available(availability))
                .map(|item| item.id.0.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            visible(Availability::default()),
            [
                "overview",
                "workspaces",
                "focused-window",
                "quick-settings",
                "night-light",
                "recording",
                "clock"
            ]
        );
        assert_eq!(
            visible(Availability {
                capture: true,
                activities: true,
                media: true,
                network: true,
                audio: true,
                notifications: true,
                battery: true,
                external_display: true
            }),
            all.iter()
                .map(|item| item.id.0.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(items(&panel).len(), 14);
    }

    #[test]
    fn settings_map_to_instances_and_group_presentation() {
        let mut panel = Panel::from_preset(PanelStyle::Floating);
        panel.center.groups[0].items[0].visible = false;
        let battery = panel.end.groups[0]
            .items
            .iter_mut()
            .find(|item| item.id.0 == "battery")
            .unwrap();
        battery.kind = ItemKind::Battery { percentage: false };
        for group in [&mut panel.start, &mut panel.center, &mut panel.end]
            .into_iter()
            .flat_map(|zone| &mut zone.groups)
        {
            group.island_padding = 13.;
        }
        assert_eq!(
            panel.center.groups[0].items[0].kind,
            ItemKind::FocusedWindow
        );
        assert!(!panel.center.groups[0].items[0].visible);
        let all = items(&panel);
        assert_eq!(
            all.iter().find(|item| item.id.0 == "battery").unwrap().kind,
            ItemKind::Battery { percentage: false }
        );
        for zone in [&panel.start, &panel.center, &panel.end] {
            for group in &zone.groups {
                assert_eq!(group.island_padding, 13.0);
            }
        }
        assert_eq!(
            all.iter()
                .find(|item| item.id.0 == "clock")
                .unwrap()
                .gap_before,
            None
        );
    }

    #[test]
    fn two_clock_instances_have_distinct_identity() {
        let first = Item::new("main-clock", ItemKind::Clock);
        let second = Item::new("other-clock", ItemKind::Clock);
        assert_eq!(first.kind, second.kind);
        assert_ne!(first.id, second.id);
    }
    #[test]
    fn removed_item_fields_are_rejected() {
        for value in [
            serde_json::json!({"id":"title", "kind":"focused-window", "enabled":false}),
            serde_json::json!({"id":"spaces", "kind":"workspaces", "workspace_style":"numbers"}),
        ] {
            assert!(serde_json::from_value::<Item>(value).is_err());
        }
    }

    #[test]
    fn surface_changes_only_replace_composition_when_item_geometry_changes() {
        let panel = Panel::default();
        let mut changed = panel.clone();
        changed.style = PanelStyle::Flat;
        assert!(panel.same_composition(&changed));
        changed.style = PanelStyle::Floating;
        assert!(!panel.same_composition(&changed));
        changed.style = PanelStyle::Flat;
        assert!(panel.same_composition(&changed));
        changed.end.groups[0].padding[1] += 1;
        assert!(!panel.same_composition(&changed));
    }
}
