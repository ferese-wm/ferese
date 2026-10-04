use ferese_ipc::theme::Snapshot;
use ferese_theme_client::service::fallback;
use serde_json::{Value, json};

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/theme-v2.json")).unwrap()
}

#[test]
fn default_snapshot_resolves_gradients_and_preserves_other_wire_values() {
    let mut snapshot = fallback();
    let wallpaper = Some(ferese_config::default_wallpaper().into());
    assert_eq!(snapshot.theme.tokens.background.path, wallpaper);
    assert_eq!(snapshot.presented.tokens.background.path, wallpaper);
    snapshot.theme.tokens.background.path = Some("<wallpaper>".into());
    snapshot.presented.tokens.background.path = Some("<wallpaper>".into());
    for theme in [&mut snapshot.theme, &mut snapshot.presented] {
        let focus = theme.tokens.focus_ring.gradient.take().unwrap();
        assert_eq!(focus.from, theme.tokens.colors.accent);
        assert_ne!(focus.from, focus.to);
        assert_eq!(focus.angle, 135.);
        let border = theme.tokens.border.gradient.take().unwrap();
        assert_eq!(border.from, theme.tokens.colors.border);
        assert_eq!(
            ferese_config::theme::rgba(&border.from).unwrap()[3],
            ferese_config::theme::rgba(&border.to).unwrap()[3],
        );
    }

    assert_eq!(serde_json::to_value(snapshot).unwrap(), fixture());
}

#[test]
fn explicit_solid_style_round_trips_without_resolving_published_values() {
    let mut value = fixture();
    for key in ["theme", "presented"] {
        value[key]["tokens"]["focus_ring"]["style"] = json!("solid");
    }

    let snapshot = Snapshot::decode(value.clone(), || panic!("unexpected catalog resolution")).unwrap();
    assert_eq!(serde_json::to_value(snapshot).unwrap(), value);
}

#[test]
fn older_snapshots_supply_catalog_but_explicit_empty_catalog_stays_empty() {
    let mut value = fixture();
    value.as_object_mut().unwrap().remove("families");
    value.as_object_mut().unwrap().remove("fallback_note");
    let snapshot: Snapshot = Snapshot::decode(value.clone(), ferese_config::families::builtins).unwrap();
    assert_eq!(snapshot.families, ferese_config::families::builtins());
    assert_eq!(snapshot.fallback_note, None);
    value["families"] = json!([]);
    assert!(
        Snapshot::decode(value.clone(), ferese_config::families::builtins)
            .unwrap()
            .families
            .is_empty()
    );
    value["families"] = Value::Null;
    assert!(Snapshot::decode(value, ferese_config::families::builtins).is_err());
}

#[test]
fn offline_preview_resolves_without_changing_published_values() {
    let before = fallback();
    let document = ferese_config::Document::parse("theme { mode \"light\"; family \"gruvbox\"; }\n").unwrap();
    let candidate = ferese_config::theme::resolve(
        &document,
        std::path::Path::new("/unused"),
        "2026-10-02T12:00:00Z".parse().unwrap(),
        |_| panic!("built-in preview must not read theme files"),
    )
    .unwrap();
    assert_eq!(candidate.theme.appearance, ferese_config::theme::Appearance::Light);
    assert_eq!(candidate.theme.tokens.colors.surface_base, "#FBF1C7");
    assert_eq!(before, fallback());
}

#[test]
fn complete_snapshots_do_not_resolve_fallbacks_and_future_versions_are_rejected() {
    let value = fixture();
    let snapshot = Snapshot::decode(value.clone(), || panic!("unexpected catalog resolution")).unwrap();
    assert_eq!(serde_json::to_value(snapshot).unwrap(), value);
    let mut future = value;
    future["version"] = json!(ferese_ipc::theme::SCHEMA_VERSION + 1);
    assert_eq!(
        Snapshot::decode(future, Vec::new).unwrap_err(),
        "Unsupported theme snapshot version"
    );
}
