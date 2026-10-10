//! Panel composition, independent of widgets, services, and Wayland surfaces.
pub mod layout;
mod radius;
use crate::BarLayout;
pub use radius::CornerRadii;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

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

pub struct Defaults {
    pub bar_layout: BarLayout,
    pub bar_island_padding: f32,
    pub window_title: bool,
    pub battery_percentage: bool,
}
impl Default for Defaults {
    fn default() -> Self {
        Self {
            bar_layout: BarLayout::Continuous,
            bar_island_padding: crate::default_bar_island_padding(),
            window_title: true,
            battery_percentage: true,
        }
    }
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

    pub fn key(self) -> &'static str {
        match self {
            Self::Numbers => "numbers",
            Self::Dots => "dots",
            Self::Tabs => "tabs",
            Self::WindowStacks => "window-stacks",
            Self::AppIcons => "app-icons",
        }
    }

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
    QuickSettings,
    Network,
    Audio,
    Recording,
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
    pub fn label(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Workspaces { .. } => "Workspaces",
            Self::FocusedWindow => "Focused window",
            Self::Media => "Media",
            Self::QuickSettings => "Quick Settings",
            Self::Network => "Network",
            Self::Audio => "Audio",
            Self::Recording => "Recording",
            Self::Notifications => "Notifications",
            Self::Battery { .. } => "Battery",
            Self::Clock => "Clock",
            Self::DisplayMode => "Display mode",
            Self::Overflow => "Overflow",
        }
    }
    pub fn representations(self) -> &'static [Representation] {
        use Representation::*;
        match self {
            Self::Media | Self::Clock => &[Wide, Compact, Icon],
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
}

impl<'de> Deserialize<'de> for Item {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Upgrade earlier drafts at the boundary. The runtime model has one
        // visibility flag and keeps kind-specific settings in ItemKind.
        let mut value = serde_json::Value::deserialize(deserializer)?;
        let fields = value
            .as_object_mut()
            .ok_or_else(|| serde::de::Error::custom("expected a panel item"))?;
        let workspaces = fields.get("kind").and_then(serde_json::Value::as_str) == Some("workspaces");
        if let Some(style) = fields.remove("workspace_style") {
            if !workspaces {
                return Err(serde::de::Error::custom(
                    "workspace-style is only supported by Workspaces items",
                ));
            }
            fields.entry("style").or_insert(style);
        }
        if fields.contains_key("style") && !workspaces {
            return Err(serde::de::Error::custom("style is only supported by Workspaces items"));
        }
        if let Some(enabled) = fields.remove("enabled") {
            if fields.get("kind").and_then(serde_json::Value::as_str) == Some("focused-window") {
                let enabled = enabled
                    .as_bool()
                    .ok_or_else(|| serde::de::Error::custom("enabled must be boolean"))?;
                let visible = fields
                    .get("visible")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true);
                fields.insert("visible".into(), (visible && enabled).into());
            }
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
        })
    }
}

impl Item {
    /// Start at the configured preference and retain supported smaller forms.
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
    pub fn new(id: &str, kind: ItemKind) -> Self {
        Self {
            id: ItemId(id.into()),
            kind,
            gap_before: None,
            representation: None,
            visible: true,
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
            } else {
                50
            },
        }
    }

    pub fn available(&self, availability: Availability) -> bool {
        if !self.visible {
            return false;
        }
        match self.kind {
            ItemKind::Media => availability.media,
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
    pub media: bool,
    pub network: bool,
    pub audio: bool,
    pub notifications: bool,
    pub battery: bool,
    pub external_display: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub id: GroupId,
    #[serde(default)]
    pub items: Vec<Item>,
    #[serde(default, rename = "surface", skip_serializing)]
    legacy_surface: serde::de::IgnoredAny,
    // Read earlier panel drafts without retaining an individual opacity setting.
    #[serde(default, rename = "background_opacity", skip_serializing)]
    legacy_background_opacity: serde::de::IgnoredAny,
    #[serde(default = "spacing")]
    pub spacing: f32,
    #[serde(default = "padding")]
    pub padding: [u16; 2],
    #[serde(default = "crate::default_bar_island_padding")]
    pub island_padding: f32,
}

impl Group {
    fn new(id: &str, items: Vec<Item>, padding: [u16; 2], island_padding: f32) -> Self {
        Self {
            id: GroupId(id.into()),
            items,
            legacy_surface: serde::de::IgnoredAny,
            legacy_background_opacity: serde::de::IgnoredAny,
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
    pub background: BarLayout,
    #[serde(default, rename = "group_surface", skip_serializing)]
    legacy_group_surface: serde::de::IgnoredAny,
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
        if panel.group_count() > MAX_GROUPS {
            return Err(format!(
                "a panel supports at most {MAX_GROUPS} groups, reserving one region for overflow"
            ));
        }
        if let Some(opacity) = panel.background_opacity
            && (!opacity.is_finite() || !(0.0..=1.0).contains(&opacity))
        {
            return Err("panel group background opacity must be between 0 and 1".into());
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
                        return Err("overflow is supplied by the panel; it cannot be configured as an item".into());
                    }
                }
            }
        }
    }
    Ok(())
}

impl Panel {
    pub fn group_count(&self) -> usize {
        [&self.start, &self.center, &self.end]
            .into_iter()
            .map(|zone| zone.groups.len())
            .sum()
    }

    /// Appearance changes do not replace item ownership or measured allocation.
    pub fn same_composition(&self, other: &Self) -> bool {
        self.id == other.id
            && self.edge == other.edge
            && self.background == other.background
            && self.start == other.start
            && self.center == other.center
            && self.end == other.end
    }

    pub fn resolved_radius(&self, shell_radius: f32) -> CornerRadii {
        self.corner_radius.unwrap_or_else(|| CornerRadii::uniform(shell_radius))
    }

    /// The overflow trigger is generated, never persisted as a configured item.
    pub fn overflow_group(&self) -> Group {
        Group {
            id: GroupId("_overflow".into()),
            items: vec![Item {
                id: ItemId("_overflow".into()),
                kind: ItemKind::Overflow,
                gap_before: None,
                representation: None,
                visible: true,
                overflow: OverflowPolicy::Never,
                priority: 100,
            }],
            legacy_surface: serde::de::IgnoredAny,
            legacy_background_opacity: serde::de::IgnoredAny,
            spacing: 0.0,
            padding: [2, 3],
            island_padding: self
                .end
                .groups
                .first()
                .map_or(crate::default_bar_island_padding(), |group| group.island_padding),
        }
    }

    pub fn item(&self, id: &ItemId) -> Option<&Item> {
        [&self.start, &self.center, &self.end]
            .into_iter()
            .flat_map(|zone| &zone.groups)
            .flat_map(|group| &group.items)
            .find(|item| &item.id == id)
    }

    /// Translate existing settings once when loading config, not on every frame.
    pub fn from_defaults(status: &Defaults) -> Self {
        use ItemKind::*;

        let item = Item::new;
        let padding = status.bar_island_padding;
        let overview = item("overview", Overview);
        let workspaces = item(
            "workspaces",
            Workspaces {
                style: WorkspaceStyle::default(),
            },
        );
        let mut title = item("focused-window", FocusedWindow);
        title.visible = status.window_title;
        let controls = vec![
            item("media", Media),
            item("quick-settings", QuickSettings),
            item("network", Network),
            item("audio", Audio),
            item("recording", Recording),
            item("notifications", Notifications),
            item(
                "battery",
                Battery {
                    percentage: status.battery_percentage,
                },
            ),
        ];
        let clock = item("clock", Clock);
        let display = item("display-mode", DisplayMode);

        let (start, center, end) = match status.bar_layout {
            BarLayout::Continuous => (
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
            BarLayout::Islands => {
                let mut controls = controls;
                controls.extend([clock, display]);
                (
                    vec![Group::new("navigation", vec![overview, workspaces], [2, 3], padding)],
                    Group::new("title", vec![title], [0, 0], padding),
                    vec![Group::new("status", controls, [2, 3], padding)],
                )
            }
        };

        Self {
            id: PanelId("main".into()),
            edge: Edge::Top,
            background: status.bar_layout,
            legacy_group_surface: serde::de::IgnoredAny,
            border: false,
            background_opacity: Option::None,
            corner_radius: Option::None,
            start: Zone {
                groups: start,
                spacing: if status.bar_layout == BarLayout::Islands {
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
    fn group_capacity_reserves_overflow_even_for_empty_groups() {
        for background in [BarLayout::Continuous, BarLayout::Islands] {
            let mut panel = Panel::from_defaults(&Defaults {
                bar_layout: background,
                ..Default::default()
            });
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
    fn removed_group_surface_is_accepted_but_not_retained() {
        for style in ["none", "inset", "island"] {
            let document =
                crate::Document::parse(&format!("panel main {{ group-surface {style}; border #true; }}")).unwrap();
            let panel: Vec<Panel> = serde_json::from_value(document.get("panels").unwrap().clone()).unwrap();
            assert!(panel[0].border);
            assert!(serde_json::to_value(&panel).unwrap()[0].get("group_surface").is_none());
        }
    }

    fn items(panel: &Panel) -> Vec<&Item> {
        [&panel.start, &panel.center, &panel.end]
            .into_iter()
            .flat_map(|zone| &zone.groups)
            .flat_map(|group| &group.items)
            .collect()
    }

    #[test]
    fn defaults_preserve_order_and_ids_across_background_changes() {
        let status = Defaults::default();
        let continuous = Panel::from_defaults(&status);
        let islands = Panel::from_defaults(&Defaults {
            bar_layout: BarLayout::Islands,
            ..status
        });
        let expected = [
            "overview",
            "workspaces",
            "focused-window",
            "media",
            "quick-settings",
            "network",
            "audio",
            "recording",
            "notifications",
            "battery",
            "clock",
            "display-mode",
        ];
        for panel in [&continuous, &islands] {
            let ids: Vec<_> = items(panel).iter().map(|item| item.id.0.as_str()).collect();
            assert_eq!(ids, expected);
            assert_eq!(
                Panel::from_defaults(&Defaults {
                    bar_layout: panel.background,
                    ..Default::default()
                }),
                *panel
            );
        }
        assert_eq!(continuous.start.groups.len(), 2);
        assert_eq!(continuous.end.groups.len(), 3);
        assert_eq!(islands.start.groups.len(), 1);
        assert_eq!(islands.end.groups.len(), 1);
        assert!(!islands.border && !continuous.border);
    }

    #[test]
    fn availability_filters_without_removing_or_reidentifying_instances() {
        let panel = Panel::from_defaults(&Defaults::default());
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
                "recording",
                "clock"
            ]
        );
        assert_eq!(
            visible(Availability {
                media: true,
                network: true,
                audio: true,
                notifications: true,
                battery: true,
                external_display: true
            }),
            all.iter().map(|item| item.id.0.as_str()).collect::<Vec<_>>()
        );
        assert_eq!(items(&panel).len(), 12);
    }

    #[test]
    fn settings_map_to_instances_and_group_presentation() {
        let panel = Panel::from_defaults(&Defaults {
            bar_layout: BarLayout::Islands,
            bar_island_padding: 13.0,
            window_title: false,
            battery_percentage: false,
        });
        assert_eq!(panel.center.groups[0].items[0].kind, ItemKind::FocusedWindow);
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
        assert_eq!(all.iter().find(|item| item.id.0 == "clock").unwrap().gap_before, None);
    }

    #[test]
    fn two_clock_instances_have_distinct_identity() {
        let first = Item::new("main-clock", ItemKind::Clock);
        let second = Item::new("other-clock", ItemKind::Clock);
        assert_eq!(first.kind, second.kind);
        assert_ne!(first.id, second.id);
    }
    #[test]
    fn composition_round_trips_kdl_and_edits_keep_ids_order_and_comments() {
        let source = r#"// panel comment
panel "main" {
    background "islands"
    end {
        group "status" {
            surface "island"
            background-opacity 0.45
            padding 2 3
            // network comment
            item "network" kind="network" overflow="always"
            item "local-clock" kind="clock"
            item "second-clock" kind="clock" representation="compact"
        }
    }
}
animations { speed 0.8; }
"#;
        let mut doc = crate::Document::parse(source).unwrap();
        let panels: Vec<Panel> = serde_json::from_value(doc.get("panels").unwrap().clone()).unwrap();
        validate(&panels).unwrap();
        assert_eq!(panels[0].end.groups[0].padding, [2, 3]);
        assert!(panels[0].background_opacity.is_none());
        assert_eq!(panels[0].end.groups[0].items[0].overflow, OverflowPolicy::Always);
        doc.set("panels.0.end.groups.0.items.0.overflow", "never".into())
            .unwrap();
        let path = "panels.0.end.groups.0.items";
        let mut items = doc.get(path).unwrap().as_array().unwrap().clone();
        items.swap(0, 2);
        doc.set(path, items.into()).unwrap();
        let formatted = doc.to_string();
        assert!(formatted.contains("// panel comment"));
        assert!(formatted.contains("// network comment"));
        let parsed = crate::Document::parse(&formatted).unwrap();
        let panels: Vec<Panel> = serde_json::from_value(parsed.get("panels").unwrap().clone()).unwrap();
        validate(&panels).unwrap();
        assert_eq!(
            panels[0].end.groups[0]
                .items
                .iter()
                .map(|item| item.id.0.as_str())
                .collect::<Vec<_>>(),
            ["second-clock", "local-clock", "network"]
        );
        assert_eq!(
            panels[0].item(&ItemId("network".into())).unwrap().overflow,
            OverflowPolicy::Never
        );
        assert_eq!(parsed.get("animations.speed").unwrap(), &serde_json::json!(0.8));
        let value = serde_json::to_value(&panels).unwrap();
        doc.set("panels", value.clone()).unwrap();
        let round_trip = crate::Document::parse(&doc.to_string()).unwrap();
        let restored: Vec<Panel> = serde_json::from_value(round_trip.get("panels").unwrap().clone()).unwrap();
        assert_eq!(restored, panels);
        doc.set(path, Vec::<serde_json::Value>::new().into()).unwrap();
        assert!(doc.to_string().contains("// network comment"));
        let restored: Vec<Panel> = serde_json::from_value(doc.get("panels").unwrap().clone()).unwrap();
        assert!(restored[0].end.groups[0].items.is_empty());
        assert_eq!(restored[0].end.spacing, 8.0);
    }

    #[test]
    fn panel_border_defaults_off_and_round_trips_an_explicit_enabled_value() {
        let mut doc =
            crate::Document::parse("panel main { end { group status { item clock kind=clock; }; }; }\n").unwrap();
        let panels: Vec<Panel> = serde_json::from_value(doc.get("panels").unwrap().clone()).unwrap();
        assert!(!panels[0].border);
        doc.set("panels.0.border", true.into()).unwrap();
        let doc = crate::Document::parse(&doc.to_string()).unwrap();
        let panels: Vec<Panel> = serde_json::from_value(doc.get("panels").unwrap().clone()).unwrap();
        assert!(panels[0].border);
        assert_eq!(panels[0].end.groups[0].items[0].id.0, "clock");
    }

    #[test]
    fn panel_background_opacity_inherits_until_overridden_and_validates_bounds() {
        let doc = crate::Document::parse("panel main { end { group status { background-opacity 0.4; }; }; }").unwrap();
        let mut panels: Vec<Panel> = serde_json::from_value(doc.get("panels").unwrap().clone()).unwrap();
        assert_eq!(panels[0].background_opacity, None);
        assert!(
            serde_json::to_value(&panels[0]).unwrap()["end"]["groups"][0]
                .get("background_opacity")
                .is_none()
        );
        for value in [0., 0.5, 1.] {
            panels[0].background_opacity = Some(value);
            validate(&panels).unwrap();
        }
        for value in [-0.1, 1.1, f32::NAN, f32::INFINITY] {
            panels[0].background_opacity = Some(value);
            assert!(validate(&panels).is_err());
        }
    }

    #[test]
    fn panel_edge_defaults_to_top_and_round_trips_both_positions() {
        for (source, expected) in [
            ("", Edge::Top),
            ("edge top;", Edge::Top),
            ("edge bottom;", Edge::Bottom),
        ] {
            let document = crate::Document::parse(&format!("panel main {{ {source} }}")).unwrap();
            let panels: Vec<Panel> = serde_json::from_value(document.get("panels").unwrap().clone()).unwrap();
            validate(&panels).unwrap();
            assert_eq!(panels[0].edge, expected);
            let restored: Vec<Panel> = serde_json::from_value(serde_json::to_value(&panels).unwrap()).unwrap();
            assert_eq!(restored[0].edge, expected);
            let mut moved = panels[0].clone();
            moved.edge = if expected == Edge::Top { Edge::Bottom } else { Edge::Top };
            assert!(!moved.same_composition(&panels[0]));
        }
    }

    #[test]
    fn validation_rejects_duplicate_ids_and_unsupported_surface_configuration() {
        let mut panel = Panel::from_defaults(&Defaults::default());
        validate(std::slice::from_ref(&panel)).unwrap();
        panel.end.groups[0].items[1].id = panel.start.groups[0].items[0].id.clone();
        assert!(validate(std::slice::from_ref(&panel)).is_err());
        panel.end.groups[0].items[1].id = ItemId("_overflow".into());
        assert!(validate(std::slice::from_ref(&panel)).is_err());
        assert!(validate(&[]).is_err());
        assert!(validate(&[panel.clone(), panel]).is_err());
        assert!(crate::from_str::<serde_json::Value>("panel \"main\" { edge \"bottom\"; }").is_ok());
        let doc = crate::Document::parse("panel \"main\" { edge \"left\"; }").unwrap();
        assert!(serde_json::from_value::<Vec<Panel>>(doc.get("panels").unwrap().clone()).is_err());
    }
    #[test]
    fn workspace_styles_round_trip_and_only_apply_to_workspaces() {
        for style in WorkspaceStyle::ALL {
            for key in ["style", "workspace-style"] {
                let source = format!(
                    "panel main {{ start {{ group nav {{ item spaces kind=\"workspaces\" {key}=\"{}\"; }} }} }}",
                    style.key()
                );
                let document = crate::Document::parse(&source).unwrap();
                let panels: Vec<Panel> = serde_json::from_value(document.get("panels").unwrap().clone()).unwrap();
                validate(&panels).unwrap();
                assert_eq!(panels[0].start.groups[0].items[0].kind, ItemKind::Workspaces { style });
                let mut wrong_kind = serde_json::to_value(&panels[0].start.groups[0].items[0]).unwrap();
                wrong_kind["kind"] = "clock".into();
                assert!(serde_json::from_value::<Item>(wrong_kind).is_err());
                let restored: Vec<Panel> = serde_json::from_value(serde_json::to_value(&panels).unwrap()).unwrap();
                assert_eq!(restored, panels);
            }
        }
        let mut item = serde_json::json!({"id": "spaces", "kind": "workspaces"});
        assert_eq!(
            serde_json::from_value::<Item>(item.clone()).unwrap().kind,
            ItemKind::Workspaces {
                style: WorkspaceStyle::Dots
            }
        );
        item["style"] = "unsupported".into();
        assert!(serde_json::from_value::<Item>(item).is_err());
    }

    #[test]
    fn legacy_title_flags_normalize_to_one_visibility_field() {
        for (visible, enabled, expected) in [
            (true, true, true),
            (true, false, false),
            (false, true, false),
            (false, false, false),
        ] {
            let item: Item = serde_json::from_value(
                serde_json::json!({ "id": "title", "kind": "focused-window", "visible": visible, "enabled": enabled }),
            )
            .unwrap();
            assert_eq!(item.kind, ItemKind::FocusedWindow);
            assert_eq!(item.visible, expected);
            let saved = serde_json::to_value(&item).unwrap();
            assert!(saved.get("enabled").is_none());
            assert_eq!(serde_json::from_value::<Item>(saved).unwrap(), item);
        }
    }
}
