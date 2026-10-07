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
        write!(formatter, "{zone} · {}", self.group.0)
    }
}

#[derive(Clone, Debug)]
pub(super) enum Action {
    Earlier(ItemId),
    Later(ItemId),
    Move(ItemId, Destination),
    Remove(ItemId),
    Set(ItemId, String, Value),
    Add(ItemKind, Destination),
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
    let zone = match destination.zone {
        "start" => &panel.start,
        "center" => &panel.center,
        "end" => &panel.end,
        _ => return Err("Unknown panel zone.".into()),
    };
    let index = zone
        .groups
        .iter()
        .position(|group| group.id == destination.group)
        .ok_or("This group no longer exists. Reload settings.")?;
    Ok(format!("panels.0.{}.groups.{index}.items", destination.zone))
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
    let panels: Vec<Panel> = serde_json::from_value(snapshot.item("panels").ok_or("Customize items first.")?.clone())
        .map_err(|error| error.to_string())?;
    ferese_config::panel::validate(&panels)?;
    let panel = &panels[0];
    let earlier = matches!(&action, Action::Earlier(_));
    let edits = match action {
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
        Action::Move(id, destination) => {
            let (source, index) = locate(panel, &id)?;
            let target = group_path(panel, &destination)?;
            if source == target {
                return Ok(Vec::new());
            }
            let mut from = records(snapshot, &source);
            let item = from.remove(index);
            let mut to = records(snapshot, &target);
            to.push(item);
            vec![set(&source, from), set(&target, to)]
        }
        Action::Remove(id) => {
            let (path, index) = locate(panel, &id)?;
            vec![Edit::Remove(path, index)]
        }
        Action::Set(id, field, value) => {
            if !matches!(
                field.as_str(),
                "visible" | "overflow" | "representation" | "percentage" | "enabled"
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
}
