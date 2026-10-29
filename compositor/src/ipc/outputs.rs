use std::sync::Arc;

use ferese_core::{OutputId, WorkspaceId};
use serde_json::Value;
use smithay::utils::{Logical, Rectangle};

use crate::backends::direct::ConnectedOutputInfo;

/// Connector data and policy borrowed for an exact, allocation-free comparison.
pub(crate) struct State<'inventory> {
    pub connected: &'inventory [ConnectedOutputInfo],
    pub focused: Option<OutputId>,
    pub error: Option<&'inventory str>,
    pub confirmation_pending: bool,
    pub low_power: bool,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Presentation {
    pub id: Option<OutputId>,
    pub geometry: Option<Rectangle<i32, Logical>>,
    pub workspace: Option<WorkspaceId>,
    pub scale: f64,
    pub transform: &'static str,
}

struct Entry {
    connected: Vec<ConnectedOutputInfo>,
    focused: Option<OutputId>,
    error: Option<String>,
    confirmation_pending: bool,
    low_power: bool,
    presentations: Vec<Presentation>,
    unlocked: Arc<Value>,
    locked: Option<Arc<Value>>,
}

#[derive(Default)]
pub(crate) struct Cache {
    entry: Option<Entry>,
}

impl Cache {
    pub(crate) fn clear(&mut self) {
        self.entry = None;
    }

    pub(crate) fn get_or_update(
        &mut self,
        state: State<'_>,
        presentations: impl Iterator<Item = Presentation> + Clone,
        locked: bool,
        build: impl FnOnce() -> Value,
    ) -> Arc<Value> {
        let unchanged = self.entry.as_ref().is_some_and(|entry| {
            entry.connected == state.connected
                && entry.focused == state.focused
                && entry.error.as_deref() == state.error
                && entry.confirmation_pending == state.confirmation_pending
                && entry.low_power == state.low_power
                && presentations
                    .clone()
                    .eq(entry.presentations.iter().copied())
        });
        if !unchanged {
            self.entry = Some(Entry {
                connected: state.connected.to_vec(),
                focused: state.focused,
                error: state.error.map(str::to_owned),
                confirmation_pending: state.confirmation_pending,
                low_power: state.low_power,
                presentations: presentations.collect(),
                unlocked: Arc::new(build()),
                locked: None,
            });
        }
        let entry = self.entry.as_mut().expect("output snapshot was populated");
        if locked {
            entry
                .locked
                .get_or_insert_with(|| {
                    let mut value = entry.unlocked.as_ref().clone();
                    redact(&mut value);
                    Arc::new(value)
                })
                .clone()
        } else {
            entry.unlocked.clone()
        }
    }
}

pub(super) fn redact(value: &mut Value) {
    let output_iter = value.as_array_mut().into_iter().flatten();
    for output in output_iter {
        if let Some(fields) = output.as_object_mut() {
            fields.remove("workspace");
            fields.remove("focused");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::direct::ConnectedModeInfo;
    use crate::config::OutputTransform;
    use serde_json::json;

    fn connector() -> ConnectedOutputInfo {
        let mode = ConnectedModeInfo {
            width: 3840,
            height: 2160,
            refresh_millihertz: 60_000,
            preferred: true,
        };
        ConnectedOutputInfo {
            connector: "HDMI-A-1".into(),
            connected: true,
            identity: "test-monitor".into(),
            internal: false,
            requested_enabled: true,
            mirror_source: None,
            enabled: true,
            profile: Some("desktop".into()),
            requested_profile: None,
            auto_refresh: true,
            adaptive_sync_supported: true,
            adaptive_sync_requested: false,
            adaptive_sync_enabled: false,
            tearing_supported: true,
            allow_tearing: false,
            physical_size: Some((600, 340)),
            current_mode: Some(mode),
            available_modes: vec![mode],
            scale: 1.75,
            transform: OutputTransform::Normal,
            configured_position: Some([0, 0]),
        }
    }

    fn state(connected: &[ConnectedOutputInfo]) -> State<'_> {
        State {
            connected,
            focused: Some(OutputId(1)),
            error: None,
            confirmation_pending: false,
            low_power: false,
        }
    }

    fn presentation() -> Presentation {
        Presentation {
            id: Some(OutputId(1)),
            geometry: Some(Rectangle::new((0, 0).into(), (2195, 1235).into())),
            workspace: Some(WorkspaceId(3)),
            scale: 1.75,
            transform: "normal",
        }
    }

    fn value() -> Value {
        json!([{"id": 1, "workspace": 3, "focused": true, "scale": 1.75}])
    }

    #[test]
    fn unchanged_outputs_reuse_storage_without_serializing_and_release_when_cleared() {
        let connected = [connector()];
        let mut cache = Cache::default();
        let first = cache.get_or_update(
            state(&connected),
            [presentation()].into_iter(),
            false,
            value,
        );
        for _ in 0..1000 {
            let next = cache.get_or_update(
                state(&connected),
                [presentation()].into_iter(),
                false,
                || panic!("unchanged outputs must not rebuild JSON"),
            );
            assert!(Arc::ptr_eq(&first, &next));
        }
        cache.clear();
        let next = cache.get_or_update(
            state(&connected),
            [presentation()].into_iter(),
            false,
            value,
        );
        assert!(!Arc::ptr_eq(&first, &next));
        assert_eq!(first, next);
    }

    #[test]
    fn lock_redaction_is_cached_without_mutating_public_or_older_snapshots() {
        let connected = [connector()];
        let mut cache = Cache::default();
        let public = cache.get_or_update(
            state(&connected),
            [presentation()].into_iter(),
            false,
            value,
        );
        let locked = cache.get_or_update(
            state(&connected),
            [presentation()].into_iter(),
            true,
            || panic!("lock redaction must reuse connector data"),
        );
        assert_eq!(*locked, json!([{"id": 1, "scale": 1.75}]));
        assert_eq!(*public, value());
        let again = cache.get_or_update(
            state(&connected),
            [presentation()].into_iter(),
            true,
            || panic!("repeated locked snapshots must not rebuild"),
        );
        assert!(Arc::ptr_eq(&locked, &again));
        let unlocked = cache.get_or_update(
            state(&connected),
            [presentation()].into_iter(),
            false,
            || panic!("unlock must reuse public data"),
        );
        assert!(Arc::ptr_eq(&public, &unlocked));
        let changed = cache.get_or_update(state(&[]), [].into_iter(), true, || json!([]));
        assert_eq!(*changed, json!([]));
        assert_eq!(*locked, json!([{"id": 1, "scale": 1.75}]));
    }

    #[test]
    fn connector_inventory_and_every_reported_property_invalidate_the_snapshot() {
        let changes: &[fn(&mut ConnectedOutputInfo)] = &[
            |info| info.connector.push('2'),
            |info| info.connected = false,
            |info| info.identity.push('2'),
            |info| info.internal = true,
            |info| info.requested_enabled = false,
            |info| info.mirror_source = Some("DP-1".into()),
            |info| info.enabled = false,
            |info| info.profile = None,
            |info| info.requested_profile = Some("mobile".into()),
            |info| info.auto_refresh = false,
            |info| info.physical_size = None,
            |info| info.current_mode = None,
            |info| info.available_modes[0].width = 1920,
            |info| info.available_modes[0].height = 1080,
            |info| info.available_modes[0].refresh_millihertz = 120_000,
            |info| info.available_modes[0].preferred = false,
            |info| info.available_modes.clear(),
            |info| info.scale = 1.5,
            |info| info.transform = OutputTransform::Rotate90,
            |info| info.configured_position = None,
        ];
        let change_iter = changes.iter().enumerate();
        for (index, change) in change_iter {
            let mut cache = Cache::default();
            let mut connected = [connector()];
            let first = cache.get_or_update(
                state(&connected),
                [presentation()].into_iter(),
                false,
                value,
            );
            change(&mut connected[0]);
            let next = cache.get_or_update(
                state(&connected),
                [presentation()].into_iter(),
                false,
                || json!({"changed": index}),
            );
            assert_eq!(*next, json!({"changed": index}));
            assert!(!Arc::ptr_eq(&first, &next));
        }
        let mut cache = Cache::default();
        let first = cache.get_or_update(
            state(&[connector()]),
            [presentation()].into_iter(),
            false,
            value,
        );
        let next = cache.get_or_update(state(&[]), [].into_iter(), false, || json!([]));
        assert_eq!(*next, json!([]));
        assert_eq!(*first, value());
    }

    #[test]
    fn focus_workspace_geometry_and_output_policy_changes_refresh_the_snapshot() {
        let connected = [connector()];
        for index in 0..10 {
            let mut cache = Cache::default();
            let first = cache.get_or_update(
                state(&connected),
                [presentation()].into_iter(),
                false,
                value,
            );
            let mut state = state(&connected);
            let mut presentation = presentation();
            match index {
                0 => state.focused = None,
                1 => state.error = Some("output rejected"),
                2 => state.confirmation_pending = true,
                3 => state.low_power = true,
                4 => presentation.id = None,
                5 => presentation.geometry = None,
                6 => presentation.geometry.as_mut().unwrap().loc.x += 100,
                7 => presentation.workspace = Some(WorkspaceId(4)),
                8 => presentation.scale = 1.25,
                9 => presentation.transform = "rotate_90",
                _ => unreachable!(),
            }
            let next = cache.get_or_update(
                state,
                [presentation].into_iter(),
                false,
                || json!({"changed": index}),
            );
            assert_eq!(*next, json!({"changed": index}));
            assert!(!Arc::ptr_eq(&first, &next));
        }
    }
}
