use ferese_config::PanelStyle;
use ferese_config::panel::*;

#[test]
fn removed_panel_surface_fields_are_rejected() {
    for source in [
        "surface solid;",
        "group-surface island;",
        "background islands;",
        "style continuous;",
        "style islands;",
    ] {
        let document =
            ferese_config::Document::parse(&format!("panel main {{ {source} }}")).unwrap();
        assert!(
            serde_json::from_value::<Vec<Panel>>(document.get("panels").unwrap().clone()).is_err(),
            "{source}"
        );
    }
}

#[test]
fn panel_surfaces_round_trip_without_group_overrides() {
    let source = "// keep\npanel main { style floating; start { group nav { item overview kind=overview; }; }; center { group title { item title kind=focused-window; }; }; end { group media { item media kind=media; }; }; }";
    let mut doc = ferese_config::Document::parse(source).unwrap();
    let panels: Vec<Panel> = serde_json::from_value(doc.get("panels").unwrap().clone()).unwrap();
    let panel = &panels[0];
    assert_eq!(panel.group_surface(), GroupSurface::Island);
    assert_eq!(panel.preset(), Some(PanelStyle::Floating));
    doc.set("panels", serde_json::to_value(&panels).unwrap())
        .unwrap();
    let restored = ferese_config::Document::parse(&doc.to_string()).unwrap();
    let restored: Vec<Panel> =
        serde_json::from_value(restored.get("panels").unwrap().clone()).unwrap();
    assert_eq!(restored, panels);
    assert!(doc.to_string().contains("// keep"));
    let mut preset = panel.clone();
    let groups = [&panel.start, &panel.center, &panel.end].map(|zone| zone.groups.clone());
    preset.apply_preset(PanelStyle::Flat);
    assert_eq!(preset.preset(), Some(PanelStyle::Flat));
    assert_eq!(
        [&preset.start, &preset.center, &preset.end].map(|zone| zone.groups.clone()),
        groups
    );
}

#[test]
fn group_surface_options_are_rejected_instead_of_inherited_or_ignored() {
    for surface in ["none", "inset", "island", "default", "unset"] {
        let doc = ferese_config::Document::parse(&format!(
            "panel main {{ start {{ group nav {{ surface {surface}; item clock kind=clock; }} }} }}"
        ))
        .unwrap();
        let error = serde_json::from_value::<Vec<Panel>>(doc.get("panels").unwrap().clone())
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown field `surface`"), "{error}");
    }
}

#[test]
fn composition_round_trips_kdl_and_edits_keep_ids_order_and_comments() {
    let source = r#"// panel comment
panel "main" {
    style "floating"
    end {
        group "status" {
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
    let mut doc = ferese_config::Document::parse(source).unwrap();
    let panels: Vec<Panel> = serde_json::from_value(doc.get("panels").unwrap().clone()).unwrap();
    validate(&panels).unwrap();
    assert_eq!(panels[0].end.groups[0].padding, [2, 3]);
    assert!(panels[0].background_opacity.is_none());
    assert_eq!(
        panels[0].end.groups[0].items[0].overflow,
        OverflowPolicy::Always
    );
    doc.set("panels.0.end.groups.0.items.0.overflow", "never".into())
        .unwrap();
    let path = "panels.0.end.groups.0.items";
    let mut items = doc.get(path).unwrap().as_array().unwrap().clone();
    items.swap(0, 2);
    doc.set(path, items.into()).unwrap();
    let formatted = doc.to_string();
    assert!(formatted.contains("// panel comment"));
    assert!(formatted.contains("// network comment"));
    let parsed = ferese_config::Document::parse(&formatted).unwrap();
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
    assert_eq!(
        parsed.get("animations.speed").unwrap(),
        &serde_json::json!(0.8)
    );
    let value = serde_json::to_value(&panels).unwrap();
    doc.set("panels", value.clone()).unwrap();
    let round_trip = ferese_config::Document::parse(&doc.to_string()).unwrap();
    let restored: Vec<Panel> =
        serde_json::from_value(round_trip.get("panels").unwrap().clone()).unwrap();
    assert_eq!(restored, panels);
    doc.set(path, Vec::<serde_json::Value>::new().into())
        .unwrap();
    assert!(doc.to_string().contains("// network comment"));
    let restored: Vec<Panel> = serde_json::from_value(doc.get("panels").unwrap().clone()).unwrap();
    assert!(restored[0].end.groups[0].items.is_empty());
    assert_eq!(restored[0].end.spacing, 8.0);
}

#[test]
fn panel_border_defaults_off_and_round_trips_an_explicit_enabled_value() {
    let mut doc = ferese_config::Document::parse(
        "panel main { end { group status { item clock kind=clock; }; }; }\n",
    )
    .unwrap();
    let panels: Vec<Panel> = serde_json::from_value(doc.get("panels").unwrap().clone()).unwrap();
    assert!(!panels[0].border);
    doc.set("panels.0.border", true.into()).unwrap();
    let doc = ferese_config::Document::parse(&doc.to_string()).unwrap();
    let panels: Vec<Panel> = serde_json::from_value(doc.get("panels").unwrap().clone()).unwrap();
    assert!(panels[0].border);
    assert_eq!(panels[0].end.groups[0].items[0].id.0, "clock");
}

#[test]
fn panel_background_opacity_inherits_until_overridden_and_validates_bounds() {
    let doc = ferese_config::Document::parse("panel main { end { group status { }; }; }").unwrap();
    let mut panels: Vec<Panel> =
        serde_json::from_value(doc.get("panels").unwrap().clone()).unwrap();
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
        let document =
            ferese_config::Document::parse(&format!("panel main {{ {source} }}")).unwrap();
        let panels: Vec<Panel> =
            serde_json::from_value(document.get("panels").unwrap().clone()).unwrap();
        validate(&panels).unwrap();
        assert_eq!(panels[0].edge, expected);
        let restored: Vec<Panel> =
            serde_json::from_value(serde_json::to_value(&panels).unwrap()).unwrap();
        assert_eq!(restored[0].edge, expected);
        let mut moved = panels[0].clone();
        moved.edge = if expected == Edge::Top {
            Edge::Bottom
        } else {
            Edge::Top
        };
        assert!(!moved.same_composition(&panels[0]));
    }
}

#[test]
fn validation_rejects_duplicate_ids_and_unsupported_surface_configuration() {
    let mut panel = Panel::default();
    validate(std::slice::from_ref(&panel)).unwrap();
    panel.end.groups[0].items[1].id = panel.start.groups[0].items[0].id.clone();
    assert!(validate(std::slice::from_ref(&panel)).is_err());
    panel.end.groups[0].items[1].id = ItemId("_overflow".into());
    assert!(validate(std::slice::from_ref(&panel)).is_err());
    assert!(validate(&[]).is_err());
    assert!(validate(&[panel.clone(), panel]).is_err());
    assert!(
        ferese_config::from_str::<serde_json::Value>("panel \"main\" { edge \"bottom\"; }").is_ok()
    );
    let doc = ferese_config::Document::parse("panel \"main\" { edge \"left\"; }").unwrap();
    assert!(serde_json::from_value::<Vec<Panel>>(doc.get("panels").unwrap().clone()).is_err());
}
#[test]
fn workspace_styles_round_trip_and_only_apply_to_workspaces() {
    for style in WorkspaceStyle::ALL {
        let source = format!(
            "panel main {{ start {{ group nav {{ item spaces kind=\"workspaces\" style=\"{}\"; }} }} }}",
            style.key()
        );
        let document = ferese_config::Document::parse(&source).unwrap();
        let panels: Vec<Panel> =
            serde_json::from_value(document.get("panels").unwrap().clone()).unwrap();
        validate(&panels).unwrap();
        assert_eq!(
            panels[0].start.groups[0].items[0].kind,
            ItemKind::Workspaces { style }
        );
        let mut wrong_kind = serde_json::to_value(&panels[0].start.groups[0].items[0]).unwrap();
        wrong_kind["kind"] = "clock".into();
        assert!(serde_json::from_value::<Item>(wrong_kind).is_err());
        let restored: Vec<Panel> =
            serde_json::from_value(serde_json::to_value(&panels).unwrap()).unwrap();
        assert_eq!(restored, panels);
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
fn item_options_are_strict_and_metadata_is_explicit() {
    for key in ["ovreflow", "visibile", "custom_note"] {
        let mut value = serde_json::json!({"id":"network", "kind":"network"});
        value[key] = "always".into();
        let error = serde_json::from_value::<Item>(value)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(key) && error.contains("unknown field"),
            "{error}"
        );
    }
    let source = "panel main { start { group nav { item network kind=\"network\" { metadata { custom-note \"keep\"; overflow \"authored note\"; }; }; }; }; }";
    let panel: Vec<Panel> = serde_json::from_value(
        ferese_config::Document::parse(source)
            .unwrap()
            .get("panels")
            .unwrap()
            .clone(),
    )
    .unwrap();
    let item = &panel[0].start.groups[0].items[0];
    assert_eq!(item.metadata["custom_note"], "keep");
    assert_eq!(item.overflow, OverflowPolicy::Auto);
    assert_eq!(
        serde_json::from_value::<Item>(serde_json::to_value(item).unwrap()).unwrap(),
        *item
    );
    assert!(
        serde_json::from_value::<Item>(
            serde_json::json!({"id":"network", "kind":"network", "metadata":"wrong"})
        )
        .is_err()
    );
}

#[test]
fn activities_are_first_class_items_in_every_zone_and_keep_overflow_settings() {
    for style in ["flat", "floating", "morph", "adaptive"] {
        for zone in ["start", "center", "end"] {
            let source = format!(
                "panel main {{ style {style}; {zone} {{ group tasks {{ item builds kind=activities priority=20 overflow=always; }}; }}; }}"
            );
            let mut doc = ferese_config::Document::parse(&source).unwrap();
            let panels: Vec<Panel> =
                serde_json::from_value(doc.get("panels").unwrap().clone()).unwrap();
            ferese_panel::validate(&panels).unwrap();
            let parsed = match zone {
                "start" => &panels[0].start,
                "center" => &panels[0].center,
                _ => &panels[0].end,
            };
            assert_eq!(
                parsed.groups[0].items[0].kind,
                ferese_panel::ItemKind::Activities
            );
            assert_eq!(
                parsed.groups[0].items[0].overflow,
                ferese_panel::OverflowPolicy::Always
            );
            doc.set("panels", serde_json::to_value(&panels).unwrap())
                .unwrap();
            let restored = ferese_config::Document::parse(&doc.to_string()).unwrap();
            assert_eq!(restored.get("panels"), doc.get("panels"));
        }
    }
}
