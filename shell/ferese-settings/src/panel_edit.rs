use ferese_config::panel::{GroupId, Item, ItemId, ItemKind, Panel};
use serde_json::Value;

use crate::store::{Edit, Snapshot, set};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Destination {
    pub zone: &'static str,
    pub group: GroupId,
}

impl std::fmt::Display for Destination {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let zone = match self.zone {
            "start" => "Start",
            "center" => "Center",
            _ => "End",
        };
        write!(
            formatter,
            "{zone} · {}",
            crate::panel_controls::group_name(&self.group.0)
        )
    }
}

#[derive(Clone, Debug)]
pub(super) enum Action {
    SetEdge(ferese_config::panel::Edge),
    SetRadius(String),
    SetCorner(usize, f32),
    SetBorder(bool),
    SetOpacity(f32),
    ClearOpacity,
    ClearRadius,
    Earlier(ItemId),
    Later(ItemId),
    Move(ItemId, Destination),
    Place(ItemId, Destination, Option<ItemId>),
    PlaceInZone(ItemId, Zone),
    Remove(ItemId),
    Set(ItemId, String, Value),
    Add(ItemKind, Destination),
    AddGroup(Zone),
    MoveGroup(GroupId, Zone),
    EarlierGroup(GroupId),
    LaterGroup(GroupId),
    RemoveGroup(GroupId),
    SetGroup(GroupId, String, Value),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Zone {
    Start,
    Center,
    End,
}

impl Zone {
    pub const ALL: [Self; 3] = [Self::Start, Self::Center, Self::End];

    pub fn key(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Center => "center",
            Self::End => "end",
        }
    }

    pub fn definition(self, panel: &Panel) -> &ferese_config::panel::Zone {
        match self {
            Self::Start => &panel.start,
            Self::Center => &panel.center,
            Self::End => &panel.end,
        }
    }
}

impl std::fmt::Display for Zone {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Start => "Start",
            Self::Center => "Center",
            Self::End => "End",
        })
    }
}

fn locate_group(panel: &Panel, id: &GroupId) -> Result<(Zone, usize), String> {
    Zone::ALL
        .into_iter()
        .find_map(|zone| {
            zone.definition(panel)
                .groups
                .iter()
                .position(|group| &group.id == id)
                .map(|index| (zone, index))
        })
        .ok_or_else(|| "This group no longer exists. Reload settings.".into())
}

pub(super) fn group_record_path(panel: &Panel, id: &GroupId) -> Result<String, String> {
    let (zone, index) = locate_group(panel, id)?;
    Ok(format!("panels.0.{}.groups.{index}", zone.key()))
}

pub(super) fn destinations(panel: &Panel) -> Vec<Destination> {
    [("start", &panel.start), ("center", &panel.center), ("end", &panel.end)]
        .into_iter()
        .flat_map(|(zone, definition)| {
            definition.groups.iter().map(move |group| Destination {
                zone,
                group: group.id.clone(),
            })
        })
        .collect()
}

fn group_path(panel: &Panel, destination: &Destination) -> Result<String, String> {
    // The zone is a display label; a queued selection follows the group's ID
    // even if another edit has since moved it to a different zone.
    Ok(format!("{}.items", group_record_path(panel, &destination.group)?))
}

fn locate(panel: &Panel, id: &ItemId) -> Result<(String, usize), String> {
    for destination in destinations(panel) {
        let path = group_path(panel, &destination)?;
        let zone = match destination.zone {
            "start" => &panel.start,
            "center" => &panel.center,
            _ => &panel.end,
        };
        let group = zone.groups.iter().find(|group| group.id == destination.group).unwrap();
        if let Some(index) = group.items.iter().position(|item| &item.id == id) {
            return Ok((path, index));
        }
    }
    Err("This item no longer exists. Reload settings.".into())
}

fn records(snapshot: &Snapshot, path: &str) -> Vec<Value> {
    snapshot
        .item(path)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Resolve stable IDs against the latest draft, then validate the complete edit
/// before handing it to Settings' existing single-save/undo path.
pub(super) fn plan(snapshot: &Snapshot, action: Action) -> Result<Vec<Edit>, String> {
    if matches!(action, Action::ClearRadius) && snapshot.item("panels.0.corner_radius").is_none() {
        return Ok(Vec::new());
    }
    // Browsing the generated composition is read-only. Materialize it together
    // with the first real edit so validation, save and Undo remain atomic.
    if snapshot.item("panels").is_none() {
        let initialize = crate::panel_controls::initialize(snapshot)?;
        let mut candidate = snapshot.clone();
        candidate.edit(&initialize)?;
        let mut edits = vec![initialize];
        edits.extend(plan(&candidate, action)?);
        return Ok(edits);
    }
    let panels: Vec<Panel> = serde_json::from_value(snapshot.item("panels").ok_or("Customize items first.")?.clone())
        .map_err(|error| error.to_string())?;
    ferese_config::panel::validate(&panels)?;
    let panel = &panels[0];
    let earlier = matches!(&action, Action::Earlier(_));
    let earlier_group = matches!(&action, Action::EarlierGroup(_));
    let edits = match action {
        Action::SetEdge(edge) => vec![set("panels.0.edge", edge.key())],
        Action::SetOpacity(value) => vec![set("panels.0.background_opacity", value)],
        Action::ClearOpacity => vec![Edit::Unset("panels.0.background_opacity".into())],
        Action::SetBorder(enabled) => vec![set("panels.0.border", enabled)],
        Action::SetCorner(index, value) => {
            let mut radii = panel.corner_radius.ok_or("Customize corners first.")?;
            *radii.0.get_mut(index).ok_or("Invalid panel corner.")? = value;
            radii.validate()?;
            vec![set("panels.0.corner_radius", radii.to_string())]
        }
        Action::SetRadius(value) => {
            let radii: ferese_config::panel::CornerRadii = value.parse()?;
            vec![set("panels.0.corner_radius", radii.to_string())]
        }
        Action::ClearRadius => vec![Edit::Unset("panels.0.corner_radius".into())],
        Action::Earlier(id) | Action::Later(id) => {
            let (path, index) = locate(panel, &id)?;
            let mut items = records(snapshot, &path);
            let next = if earlier {
                index.checked_sub(1)
            } else {
                (index + 1 < items.len()).then_some(index + 1)
            };
            let Some(next) = next else { return Ok(Vec::new()) };
            items.swap(index, next);
            vec![set(&path, items)]
        }
        Action::Place(id, destination, before) => {
            let (source, index) = locate(panel, &id)?;
            let target = group_path(panel, &destination)?;
            if before.as_ref() == Some(&id) && source == target {
                return Ok(Vec::new());
            }
            let mut from = records(snapshot, &source);
            let original = from.clone();
            let item = from.remove(index);
            let mut to = if source == target {
                from.clone()
            } else {
                records(snapshot, &target)
            };
            let insertion = match before {
                Some(before) => to
                    .iter()
                    .position(|item| item["id"].as_str() == Some(&before.0))
                    .ok_or("The drop target changed. Try dragging again.")?,
                None => to.len(),
            };
            to.insert(insertion, item);
            if source == target {
                if to == original {
                    Vec::new()
                } else {
                    vec![set(&target, to)]
                }
            } else {
                vec![set(&source, from), set(&target, to)]
            }
        }
        Action::Move(id, destination) => {
            let (source, _) = locate(panel, &id)?;
            if source == group_path(panel, &destination)? {
                return Ok(Vec::new());
            }
            return plan(snapshot, Action::Place(id, destination, None));
        }
        Action::PlaceInZone(id, zone) => {
            if let Some(group) = zone.definition(panel).groups.last() {
                return plan(
                    snapshot,
                    Action::Place(
                        id,
                        Destination {
                            zone: zone.key(),
                            group: group.id.clone(),
                        },
                        None,
                    ),
                );
            }
            let mut edits = plan(snapshot, Action::AddGroup(zone))?;
            let mut candidate = snapshot.clone();
            for edit in &edits {
                candidate.edit(edit)?;
            }
            let group = records(&candidate, &format!("panels.0.{}.groups", zone.key()))[0]["id"]
                .as_str()
                .unwrap()
                .to_owned();
            edits.extend(plan(
                &candidate,
                Action::Place(
                    id,
                    Destination {
                        zone: zone.key(),
                        group: GroupId(group),
                    },
                    None,
                ),
            )?);
            edits
        }
        Action::Remove(id) => {
            let (path, index) = locate(panel, &id)?;
            vec![Edit::Remove(path, index)]
        }
        Action::Set(id, field, value) => {
            if !matches!(
                field.as_str(),
                "visible" | "overflow" | "representation" | "percentage" | "enabled" | "workspace_style"
            ) {
                return Err("Unknown panel item setting.".into());
            }
            let (path, index) = locate(panel, &id)?;
            vec![set(&format!("{path}.{index}.{field}"), value)]
        }
        Action::Add(kind, destination) => {
            if kind == ItemKind::Overflow {
                return Err("Overflow is supplied by the panel.".into());
            }
            let path = group_path(panel, &destination)?;
            let name = serde_json::to_value(kind).map_err(|error| error.to_string())?;
            let name = name["kind"].as_str().unwrap();
            let count = destinations(panel)
                .iter()
                .map(|destination| {
                    let path = group_path(panel, destination).unwrap();
                    records(snapshot, &path).len()
                })
                .sum::<usize>();
            let id = (1..=count + 1)
                .map(|number| {
                    if number == 1 {
                        name.to_owned()
                    } else {
                        format!("{name}-{number}")
                    }
                })
                .find(|id| panel.item(&ItemId(id.clone())).is_none())
                .unwrap();
            let mut items = records(snapshot, &path);
            items.push(serde_json::to_value(Item::new(&id, kind)).map_err(|error| error.to_string())?);
            vec![set(&path, items)]
        }
        Action::AddGroup(zone) => {
            let count = Zone::ALL
                .iter()
                .map(|zone| zone.definition(panel).groups.len())
                .sum::<usize>();
            let id = (1..=count + 1)
                .map(|number| format!("{}-{number}", zone.key()))
                .find(|id| locate_group(panel, &GroupId(id.clone())).is_err())
                .unwrap();
            let path = format!("panels.0.{}.groups", zone.key());
            let mut groups = records(snapshot, &path);
            groups.push(serde_json::json!({
                "id": id,
                "items": []
            }));
            vec![set(&path, groups)]
        }
        Action::MoveGroup(id, target) => {
            let (source, index) = locate_group(panel, &id)?;
            if source == target {
                return Ok(Vec::new());
            }
            let source = format!("panels.0.{}.groups", source.key());
            let target = format!("panels.0.{}.groups", target.key());
            let mut from = records(snapshot, &source);
            let mut to = records(snapshot, &target);
            to.push(from.remove(index));
            vec![set(&source, from), set(&target, to)]
        }
        Action::EarlierGroup(id) | Action::LaterGroup(id) => {
            let (zone, index) = locate_group(panel, &id)?;
            let path = format!("panels.0.{}.groups", zone.key());
            let mut groups = records(snapshot, &path);
            let next = if earlier_group {
                index.checked_sub(1)
            } else {
                (index + 1 < groups.len()).then_some(index + 1)
            };
            let Some(next) = next else { return Ok(Vec::new()) };
            groups.swap(index, next);
            vec![set(&path, groups)]
        }
        Action::RemoveGroup(id) => {
            let (zone, index) = locate_group(panel, &id)?;
            if !zone.definition(panel).groups[index].items.is_empty() {
                return Err("Move or remove this group's items before removing the group.".into());
            }
            vec![Edit::Remove(format!("panels.0.{}.groups", zone.key()), index)]
        }
        Action::SetGroup(id, field, value) => {
            let (field, value) = match field.as_str() {
                "padding_vertical" | "padding_horizontal" => {
                    let (zone, index) = locate_group(panel, &id)?;
                    let mut padding = serde_json::to_value(zone.definition(panel).groups[index].padding).unwrap();
                    padding[usize::from(field == "padding_horizontal")] = value;
                    ("padding".to_owned(), padding)
                }
                _ => (field, value),
            };
            if !matches!(field.as_str(), "spacing" | "padding" | "island_padding") {
                return Err("Unknown panel group setting.".into());
            }
            vec![set(&format!("{}.{field}", group_record_path(panel, &id)?), value)]
        }
    };
    let mut candidate = snapshot.clone();
    for edit in &edits {
        candidate.edit(edit)?;
    }
    Snapshot::parse(candidate.source)?;
    Ok(edits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_capacity_is_checked_before_saving_and_removal_reopens_a_slot() {
        let mut snapshot = Snapshot::parse("panel main {}".into()).unwrap();
        for _ in 0..ferese_config::panel::MAX_GROUPS {
            apply(&mut snapshot, Action::AddGroup(Zone::End));
        }
        let source = snapshot.source.clone();
        assert!(
            plan(&snapshot, Action::AddGroup(Zone::Start))
                .unwrap_err()
                .contains("31 groups")
        );
        assert_eq!(snapshot.source, source);
        apply(&mut snapshot, Action::RemoveGroup(GroupId("end-1".into())));
        apply(&mut snapshot, Action::AddGroup(Zone::Start));
        assert_eq!(snapshot.records("panels.0.start.groups"), 1);
    }

    fn snapshot() -> Snapshot {
        Snapshot::parse(
            r#"// preserve config
panel "main" {
    start { group "navigation" { item "overview" kind="overview"; }; }
    center { group "title" { item "title" kind="focused-window"; }; }
    end { group "status" {
        // first clock
        item "clock" kind="clock" custom-note="keep"
        item "clock-2" kind="clock"
        item "network" kind="network" overflow="always"
    }; }
}
animations { speed 0.8; }
"#
            .into(),
        )
        .unwrap()
    }
    fn apply(snapshot: &mut Snapshot, action: Action) {
        for edit in plan(snapshot, action).unwrap() {
            snapshot.edit(&edit).unwrap();
        }
        *snapshot = Snapshot::parse(snapshot.source.clone()).unwrap();
    }
    fn ids(snapshot: &Snapshot, path: &str) -> Vec<String> {
        records(snapshot, path)
            .iter()
            .map(|record| record["id"].as_str().unwrap().to_owned())
            .collect()
    }
    fn target(zone: &'static str, group: &str) -> Destination {
        Destination {
            zone,
            group: GroupId(group.into()),
        }
    }

    #[test]
    fn shared_group_style_and_opacity_edit_only_the_panel_and_can_restore_inheritance() {
        let mut snapshot = Snapshot::parse(String::new()).unwrap();
        apply(&mut snapshot, Action::SetBorder(false));
        let groups = snapshot.item("panels.0.end.groups").unwrap().clone();
        apply(&mut snapshot, Action::SetOpacity(0.6));
        assert!((snapshot.number("panels.0.background_opacity", 0.) - 0.6).abs() < 0.0001);
        assert!(!snapshot.boolean("panels.0.border", true));
        assert_eq!(snapshot.item("panels.0.end.groups"), Some(&groups));
        let source = snapshot.source.clone();
        for value in [-0.1, 1.1, f32::NAN] {
            assert!(plan(&snapshot, Action::SetOpacity(value)).is_err());
            assert_eq!(snapshot.source, source);
        }
        apply(&mut snapshot, Action::ClearOpacity);
        assert!(snapshot.item("panels.0.background_opacity").is_none());
        assert_eq!(snapshot.item("panels.0.end.groups"), Some(&groups));
    }

    #[test]
    fn border_toggle_materializes_once_and_preserves_every_group() {
        let mut snapshot = Snapshot::parse("status { bar-layout islands; }".into()).unwrap();
        let original: Vec<Panel> = {
            let Edit::Set(_, value) = crate::panel_controls::initialize(&snapshot).unwrap() else {
                unreachable!()
            };
            serde_json::from_value(value).unwrap()
        };
        apply(&mut snapshot, Action::SetBorder(false));
        let panels: Vec<Panel> = serde_json::from_value(snapshot.item("panels").unwrap().clone()).unwrap();
        assert!(!panels[0].border);
        assert_eq!(panels[0].start, original[0].start);
        assert_eq!(panels[0].center, original[0].center);
        assert_eq!(panels[0].end, original[0].end);
        apply(&mut snapshot, Action::SetBorder(true));
        assert!(snapshot.boolean("panels.0.border", false));
    }

    #[test]
    fn corner_edits_preserve_other_corners_and_validate_before_publication() {
        let mut snapshot = Snapshot::parse(String::new()).unwrap();
        apply(&mut snapshot, Action::SetRadius("4px 8px 12px 16px".into()));
        apply(&mut snapshot, Action::SetCorner(0, 9.));
        apply(&mut snapshot, Action::SetCorner(3, 21.));
        let radii: ferese_config::panel::CornerRadii =
            serde_json::from_value(snapshot.item("panels.0.corner_radius").unwrap().clone()).unwrap();
        assert_eq!(radii.0, [9., 8., 12., 21.]);
        let previous = snapshot.source.clone();
        for action in [
            Action::SetCorner(4, 3.),
            Action::SetCorner(0, -1.),
            Action::SetCorner(1, f32::NAN),
        ] {
            assert!(plan(&snapshot, action).is_err());
            assert_eq!(snapshot.source, previous);
        }
    }

    #[test]
    fn panel_corner_override_is_independent_and_clearing_or_removing_restores_inheritance() {
        let mut snapshot = Snapshot::parse("theme { geometry { shell-radius 14; }; }".into()).unwrap();
        assert!(plan(&snapshot, Action::ClearRadius).unwrap().is_empty());
        apply(&mut snapshot, Action::SetRadius("4px 8px 12px 16px".into()));
        let resolve = |snapshot: &Snapshot, radius| {
            let panels: Vec<Panel> = serde_json::from_value(snapshot.item("panels").unwrap().clone()).unwrap();
            panels[0].resolved_radius(radius).0
        };
        assert_eq!(resolve(&snapshot, 14.), [4., 8., 12., 16.]);
        snapshot.edit(&set("theme.geometry.shell_radius", 20)).unwrap();
        assert_eq!(resolve(&snapshot, 20.), [4., 8., 12., 16.]);
        let original = snapshot.source.clone();
        assert!(plan(&snapshot, Action::SetRadius("-4px".into())).is_err());
        assert_eq!(snapshot.source, original);
        apply(&mut snapshot, Action::ClearRadius);
        assert_eq!(resolve(&snapshot, 20.), [20.; 4]);
        apply(&mut snapshot, Action::SetRadius("8px".into()));
        snapshot.edit(&Edit::Unset("panels".into())).unwrap();
        let Edit::Set(_, panel) = crate::panel_controls::initialize(&snapshot).unwrap() else {
            unreachable!()
        };
        let panels: Vec<Panel> = serde_json::from_value(panel).unwrap();
        assert_eq!(panels[0].resolved_radius(20.).0, [20.; 4]);
        assert_eq!(snapshot.number("theme.geometry.shell_radius", 0.), 20.);
    }

    #[test]
    fn changing_edge_materializes_preferences_and_round_trips_without_moving_items() {
        let mut snapshot = Snapshot::parse("status { battery-percentage #false; }".into()).unwrap();
        apply(&mut snapshot, Action::SetEdge(ferese_config::panel::Edge::Bottom));
        let before = snapshot.item("panels.0.start").unwrap().clone();
        assert_eq!(snapshot.string("panels.0.edge", ""), "bottom");
        assert!(!snapshot.boolean("panels.0.end.groups.0.items.6.percentage", true));
        apply(&mut snapshot, Action::SetEdge(ferese_config::panel::Edge::Top));
        assert_eq!(snapshot.string("panels.0.edge", ""), "top");
        assert_eq!(snapshot.item("panels.0.start"), Some(&before));
        Snapshot::parse(snapshot.source).unwrap();
    }

    #[test]
    fn first_edit_materializes_legacy_settings_and_the_change_together() {
        let mut snapshot = Snapshot::parse("// preserved\nstatus { battery-percentage #false; }\n".into()).unwrap();
        let original = snapshot.source.clone();
        assert!(
            plan(
                &snapshot,
                Action::Set(ItemId("missing".into()), "visible".into(), false.into())
            )
            .is_err()
        );
        assert_eq!(snapshot.source, original);
        let edits = plan(
            &snapshot,
            Action::Set(ItemId("battery".into()), "visible".into(), false.into()),
        )
        .unwrap();
        assert_eq!(edits.len(), 2);
        for edit in edits {
            snapshot.edit(&edit).unwrap();
        }
        assert!(!snapshot.boolean("panels.0.end.groups.0.items.6.visible", true));
        assert!(!snapshot.boolean("panels.0.end.groups.0.items.6.percentage", true));
        assert!(snapshot.source.contains("// preserved"));
        Snapshot::parse(snapshot.source).unwrap();
    }

    #[test]
    fn reordering_and_cross_zone_moves_keep_identity_settings_and_custom_fields() {
        let mut snapshot = snapshot();
        apply(&mut snapshot, Action::Earlier(ItemId("network".into())));
        assert_eq!(
            ids(&snapshot, "panels.0.end.groups.0.items"),
            ["clock", "network", "clock-2"]
        );
        apply(
            &mut snapshot,
            Action::Move(ItemId("clock".into()), target("start", "navigation")),
        );
        assert_eq!(ids(&snapshot, "panels.0.start.groups.0.items"), ["overview", "clock"]);
        assert_eq!(
            snapshot.string("panels.0.start.groups.0.items.1.custom_note", ""),
            "keep"
        );
        assert_eq!(snapshot.string("panels.0.end.groups.0.items.0.overflow", ""), "always");
        assert!(snapshot.source.contains("// first clock"));
        assert_eq!(snapshot.number("animations.speed", 1.0), 0.8);
    }

    #[test]
    fn repeated_kind_additions_get_unique_ids_and_remove_only_the_addressed_instance() {
        let mut snapshot = snapshot();
        apply(&mut snapshot, Action::Add(ItemKind::Clock, target("end", "status")));
        assert_eq!(
            ids(&snapshot, "panels.0.end.groups.0.items"),
            ["clock", "clock-2", "network", "clock-3"]
        );
        apply(&mut snapshot, Action::Remove(ItemId("clock-2".into())));
        assert_eq!(
            ids(&snapshot, "panels.0.end.groups.0.items"),
            ["clock", "network", "clock-3"]
        );
        apply(&mut snapshot, Action::Add(ItemKind::Clock, target("end", "status")));
        assert_eq!(
            ids(&snapshot, "panels.0.end.groups.0.items"),
            ["clock", "network", "clock-3", "clock-2"]
        );
    }

    #[test]
    fn pending_actions_resolve_ids_after_a_prior_move_and_boundaries_are_noops() {
        let mut snapshot = snapshot();
        let pending = Action::Later(ItemId("network".into()));
        apply(
            &mut snapshot,
            Action::Move(ItemId("network".into()), target("start", "navigation")),
        );
        assert!(plan(&snapshot, pending).unwrap().is_empty());
        apply(&mut snapshot, Action::Earlier(ItemId("network".into())));
        assert_eq!(ids(&snapshot, "panels.0.start.groups.0.items"), ["network", "overview"]);
        assert!(
            plan(&snapshot, Action::Earlier(ItemId("network".into())))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn item_setting_changes_follow_the_instance_after_reordering() {
        let mut snapshot = snapshot();
        let pending = Action::Set(ItemId("network".into()), "overflow".into(), "never".into());
        apply(
            &mut snapshot,
            Action::Move(ItemId("network".into()), target("start", "navigation")),
        );
        apply(&mut snapshot, pending);
        assert_eq!(snapshot.string("panels.0.start.groups.0.items.1.id", ""), "network");
        assert_eq!(snapshot.string("panels.0.start.groups.0.items.1.overflow", ""), "never");
        assert_eq!(snapshot.string("panels.0.end.groups.0.items.1.id", ""), "clock-2");
        assert!(
            plan(
                &snapshot,
                Action::Set(ItemId("clock".into()), "visible".into(), "invalid".into())
            )
            .is_err()
        );
    }

    #[test]
    fn invalid_targets_do_not_change_the_draft_and_last_item_removal_keeps_an_empty_group() {
        let mut snapshot = snapshot();
        let original = snapshot.source.clone();
        assert!(
            plan(
                &snapshot,
                Action::Move(ItemId("clock".into()), target("start", "missing"))
            )
            .is_err()
        );
        assert!(plan(&snapshot, Action::Remove(ItemId("missing".into()))).is_err());
        assert_eq!(snapshot.source, original);
        apply(&mut snapshot, Action::Remove(ItemId("title".into())));
        assert_eq!(snapshot.records("panels.0.center.groups"), 1);
        assert_eq!(snapshot.records("panels.0.center.groups.0.items"), 0);
        apply(&mut snapshot, Action::Add(ItemKind::Clock, target("center", "title")));
        assert_eq!(snapshot.records("panels.0.center.groups.0.items"), 1);
    }

    #[test]
    fn the_whole_move_is_validated_before_a_save_and_an_undo_restores_composition() {
        let mut snapshot = snapshot();
        let original = snapshot.clone();
        let edits = plan(
            &snapshot,
            Action::Move(ItemId("clock".into()), target("center", "title")),
        )
        .unwrap();
        assert_eq!(edits.len(), 2);
        for edit in edits {
            snapshot.edit(&edit).unwrap();
        }
        let complete = Snapshot::parse(snapshot.source).unwrap();
        assert_eq!(ids(&complete, "panels.0.center.groups.0.items"), ["title", "clock"]);
        let restored = Snapshot::parse(original.source).unwrap();
        assert_eq!(restored.doc.value(), original.doc.value());
    }

    #[test]
    fn groups_move_and_reorder_without_losing_item_identity_or_authored_data() {
        let mut snapshot = snapshot();
        let id = GroupId("status".into());
        apply(&mut snapshot, Action::SetBorder(false));
        apply(
            &mut snapshot,
            Action::SetGroup(id.clone(), "spacing".into(), 7.5.into()),
        );
        apply(&mut snapshot, Action::SetOpacity(0.45));
        apply(&mut snapshot, Action::MoveGroup(id.clone(), Zone::Start));
        apply(&mut snapshot, Action::EarlierGroup(id.clone()));
        assert_eq!(ids(&snapshot, "panels.0.start.groups"), ["status", "navigation"]);
        assert_eq!(
            ids(&snapshot, "panels.0.start.groups.0.items"),
            ["clock", "clock-2", "network"]
        );
        assert_eq!(
            snapshot.string("panels.0.start.groups.0.items.0.custom_note", ""),
            "keep"
        );
        assert_eq!(snapshot.number("panels.0.start.groups.0.spacing", 0.), 7.5);
        assert!((snapshot.number("panels.0.background_opacity", 1.) - 0.45).abs() < 0.0001);
        assert!(snapshot.source.contains("// first clock"));
        assert!(plan(&snapshot, Action::EarlierGroup(id.clone())).unwrap().is_empty());
        apply(&mut snapshot, Action::LaterGroup(id.clone()));
        assert_eq!(ids(&snapshot, "panels.0.start.groups"), ["navigation", "status"]);
        assert!(plan(&snapshot, Action::LaterGroup(id.clone())).unwrap().is_empty());
        assert!(plan(&snapshot, Action::MoveGroup(id, Zone::Start)).unwrap().is_empty());
        assert_eq!(snapshot.number("animations.speed", 1.), 0.8);
    }

    #[test]
    fn queued_item_and_group_edits_follow_groups_across_zones() {
        let mut snapshot = snapshot();
        let id = GroupId("status".into());
        let pending = Action::Add(ItemKind::Clock, target("end", "status"));
        apply(&mut snapshot, Action::MoveGroup(id.clone(), Zone::Center));
        apply(&mut snapshot, pending);
        apply(
            &mut snapshot,
            Action::SetGroup(id.clone(), "padding_vertical".into(), 9.into()),
        );
        apply(
            &mut snapshot,
            Action::SetGroup(id, "padding_horizontal".into(), 11.into()),
        );
        assert_eq!(
            snapshot.item("panels.0.center.groups.1.padding"),
            Some(&serde_json::json!([9, 11]))
        );
        assert_eq!(
            ids(&snapshot, "panels.0.center.groups.1.items"),
            ["clock", "clock-2", "network", "clock-3"]
        );
        apply(
            &mut snapshot,
            Action::Move(ItemId("overview".into()), target("end", "status")),
        );
        assert_eq!(
            ids(&snapshot, "panels.0.center.groups.1.items"),
            ["clock", "clock-2", "network", "clock-3", "overview"]
        );
    }

    #[test]
    fn empty_zones_can_gain_unique_groups_and_nonempty_groups_cannot_be_removed() {
        let mut snapshot = Snapshot::parse("panel \"main\" { background islands; }".into()).unwrap();
        for _ in 0..3 {
            apply(&mut snapshot, Action::AddGroup(Zone::End));
        }
        assert_eq!(ids(&snapshot, "panels.0.end.groups"), ["end-1", "end-2", "end-3"]);
        apply(&mut snapshot, Action::SetBorder(false));
        assert!(!snapshot.boolean("panels.0.border", true));
        apply(&mut snapshot, Action::Add(ItemKind::Clock, target("end", "end-2")));
        let original = snapshot.source.clone();
        assert!(plan(&snapshot, Action::RemoveGroup(GroupId("end-2".into()))).is_err());
        assert_eq!(snapshot.source, original);
        apply(&mut snapshot, Action::RemoveGroup(GroupId("end-1".into())));
        assert_eq!(ids(&snapshot, "panels.0.end.groups"), ["end-2", "end-3"]);
        apply(&mut snapshot, Action::Remove(ItemId("clock".into())));
        apply(&mut snapshot, Action::RemoveGroup(GroupId("end-2".into())));
        apply(&mut snapshot, Action::RemoveGroup(GroupId("end-3".into())));
        apply(&mut snapshot, Action::AddGroup(Zone::Start));
        assert_eq!(ids(&snapshot, "panels.0.start.groups"), ["start-1"]);
    }

    #[test]
    fn invalid_group_settings_fail_before_mutating_the_draft() {
        let snapshot = snapshot();
        let original = snapshot.source.clone();
        for (field, value) in [
            ("spacing", serde_json::json!(65)),
            ("background_opacity", serde_json::json!(-0.1)),
            ("background_opacity", serde_json::json!(1.1)),
            ("padding_horizontal", serde_json::json!(33)),
            ("surface", serde_json::json!("unknown")),
            ("items", serde_json::json!([])),
        ] {
            assert!(
                plan(
                    &snapshot,
                    Action::SetGroup(GroupId("status".into()), field.into(), value)
                )
                .is_err()
            );
        }
        assert!(plan(&snapshot, Action::MoveGroup(GroupId("missing".into()), Zone::Start)).is_err());
        assert_eq!(snapshot.source, original);
    }

    #[test]
    fn drop_reorders_exact_instances_and_preserves_authored_fields() {
        let mut snapshot = snapshot();
        let id = ItemId("clock".into());
        apply(&mut snapshot, Action::Place(id.clone(), target("end", "status"), None));
        assert_eq!(
            ids(&snapshot, "panels.0.end.groups.0.items"),
            ["clock-2", "network", "clock"]
        );
        assert_eq!(snapshot.string("panels.0.end.groups.0.items.2.custom_note", ""), "keep");
        assert!(
            plan(&snapshot, Action::Place(id.clone(), target("end", "status"), None))
                .unwrap()
                .is_empty()
        );
        assert!(
            plan(
                &snapshot,
                Action::Place(id.clone(), target("end", "status"), Some(id.clone()))
            )
            .unwrap()
            .is_empty()
        );
        apply(
            &mut snapshot,
            Action::Place(id.clone(), target("end", "status"), Some(ItemId("clock-2".into()))),
        );
        assert_eq!(
            ids(&snapshot, "panels.0.end.groups.0.items"),
            ["clock", "clock-2", "network"]
        );
        apply(
            &mut snapshot,
            Action::Place(id, target("center", "title"), Some(ItemId("title".into()))),
        );
        assert_eq!(ids(&snapshot, "panels.0.center.groups.0.items"), ["clock", "title"]);
        assert_eq!(ids(&snapshot, "panels.0.end.groups.0.items"), ["clock-2", "network"]);
        assert!(snapshot.source.contains("// first clock"));
    }

    #[test]
    fn stale_drop_targets_reject_the_whole_edit_and_empty_zone_drop_creates_one_group() {
        let mut snapshot = snapshot();
        let original = snapshot.source.clone();
        assert!(
            plan(
                &snapshot,
                Action::Place(
                    ItemId("clock".into()),
                    target("start", "navigation"),
                    Some(ItemId("missing".into()))
                )
            )
            .is_err()
        );
        assert_eq!(snapshot.source, original);
        apply(&mut snapshot, Action::Remove(ItemId("title".into())));
        apply(&mut snapshot, Action::RemoveGroup(GroupId("title".into())));
        let edits = plan(&snapshot, Action::PlaceInZone(ItemId("clock".into()), Zone::Center)).unwrap();
        assert_eq!(edits.len(), 3, "create and move through one save");
        for edit in edits {
            snapshot.edit(&edit).unwrap();
        }
        let snapshot = Snapshot::parse(snapshot.source).unwrap();
        assert_eq!(snapshot.records("panels.0.center.groups"), 1);
        assert_eq!(ids(&snapshot, "panels.0.center.groups.0.items"), ["clock"]);
        assert_eq!(
            snapshot.string("panels.0.center.groups.0.items.0.custom_note", ""),
            "keep"
        );
    }
    #[test]
    fn workspace_style_selection_preserves_other_preferences_and_moves_with_the_item() {
        let mut snapshot = Snapshot::parse("// keep\nstatus { battery-percentage #false; }".into()).unwrap();
        for style in ferese_config::panel::WorkspaceStyle::ALL {
            apply(
                &mut snapshot,
                Action::Set(
                    ItemId("workspaces".into()),
                    "workspace_style".into(),
                    style.key().into(),
                ),
            );
            let panels: Vec<Panel> = serde_json::from_value(snapshot.item("panels").unwrap().clone()).unwrap();
            let panel = &panels[0];
            assert_eq!(
                panel.item(&ItemId("workspaces".into())).unwrap().workspace_style,
                Some(style)
            );
            assert!(snapshot.source.contains("// keep"));
            assert_eq!(
                panel.item(&ItemId("battery".into())).unwrap().kind,
                ItemKind::Battery { percentage: false }
            );
            Snapshot::parse(snapshot.source.clone()).unwrap();
        }
        apply(
            &mut snapshot,
            Action::Move(ItemId("workspaces".into()), target("end", "status")),
        );
        let panels: Vec<Panel> = serde_json::from_value(snapshot.item("panels").unwrap().clone()).unwrap();
        let panel = &panels[0];
        assert_eq!(
            panel.item(&ItemId("workspaces".into())).unwrap().workspace_style,
            Some(ferese_config::panel::WorkspaceStyle::AppIcons)
        );
    }
}
