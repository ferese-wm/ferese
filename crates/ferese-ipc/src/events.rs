use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Event schema version, independent of command framing versions.
pub const VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ConfigState {
    pub revision: u64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct LockState {
    pub phase: String,
    pub sleeping: bool,
}

/// Window metadata included in desktop event snapshots.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Window {
    pub id: u64,
    pub app_id: String,
    pub title: String,
    pub focused: bool,
    pub workspace: u64,
    pub floating: bool,
    pub fullscreen: bool,
}

/// A coherent logical desktop view. Desktop objects are redacted while locked.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Snapshot {
    pub outputs: Arc<Value>,
    pub workspaces: Value,
    pub windows: Vec<Window>,
    pub focus: Value,
    pub config: ConfigState,
    pub theme: Arc<Value>,
    pub lock: LockState,
}

/// Replacement values for each changed domain; not animation frames or client damage.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Change {
    Snapshot { desktop: Box<Snapshot> },
    OutputsChanged { outputs: Value },
    WorkspacesChanged { workspaces: Value },
    WindowsChanged { windows: Vec<Window> },
    FocusChanged { focus: Value },
    ConfigChanged { config: ConfigState },
    ThemeChanged { theme: Value },
    LockChanged { lock: LockState },
}

/// All changes from one dispatch share a generation. The last marks its end.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Event {
    pub version: u32,
    pub generation: u64,
    pub last: bool,
    #[serde(flatten)]
    pub change: Change,
}

impl Event {
    pub fn validate(&self) -> Result<(), String> {
        if self.version == VERSION {
            Ok(())
        } else {
            Err(format!("Unsupported IPC event version {}", self.version))
        }
    }
}

impl Snapshot {
    #[must_use]
    pub fn changes_since(&self, previous: &Self) -> Vec<Change> {
        let mut changes = Vec::new();
        if !Arc::ptr_eq(&self.outputs, &previous.outputs) && self.outputs != previous.outputs {
            changes.push(Change::OutputsChanged {
                outputs: self.outputs.as_ref().clone(),
            });
        }
        if self.workspaces != previous.workspaces {
            changes.push(Change::WorkspacesChanged {
                workspaces: self.workspaces.clone(),
            });
        }
        if self.windows != previous.windows {
            changes.push(Change::WindowsChanged {
                windows: self.windows.clone(),
            });
        }
        if self.focus != previous.focus {
            changes.push(Change::FocusChanged {
                focus: self.focus.clone(),
            });
        }
        if self.config != previous.config {
            changes.push(Change::ConfigChanged {
                config: self.config.clone(),
            });
        }
        if !Arc::ptr_eq(&self.theme, &previous.theme) && self.theme != previous.theme {
            changes.push(Change::ThemeChanged {
                theme: self.theme.as_ref().clone(),
            });
        }
        if self.lock != previous.lock {
            changes.push(Change::LockChanged {
                lock: self.lock.clone(),
            });
        }
        changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_outputs_preserve_wire_fields_and_detect_value_changes() {
        let outputs = serde_json::json!([{
            "id": 1, "connector": "HDMI-A-1", "scale": 1.75,
            "available_modes": [{"width": 3840, "height": 2160, "preferred": true}],
            "configuration_error": null
        }]);
        let wire = serde_json::json!({
            "outputs": outputs, "workspaces": [], "windows": [], "focus": null,
            "config": {"revision": 0, "error": null}, "theme": null,
            "lock": {"phase": "unlocked", "sleeping": false}
        });
        let snapshot: Snapshot = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(&snapshot).unwrap(), wire);
        let mut next = snapshot.clone();
        assert!(Arc::ptr_eq(&snapshot.outputs, &next.outputs));
        assert!(next.changes_since(&snapshot).is_empty());
        next.outputs = Arc::new(outputs.clone());
        assert!(next.changes_since(&snapshot).is_empty());
        next.outputs = serde_json::json!([]).into();
        let changes = next.changes_since(&snapshot);
        assert_eq!(
            serde_json::to_value(&changes).unwrap(),
            serde_json::json!([
                {"type": "outputs_changed", "outputs": []}
            ])
        );
        assert_eq!(*snapshot.outputs, outputs);
    }

    #[test]
    fn typed_windows_preserve_snapshot_and_replacement_wire_fields() {
        let windows = serde_json::json!([{
            "id": u64::MAX,
            "app_id": "org.example.Editor.desktop",
            "title": "Chapter \"one\"\n第二课",
            "focused": true,
            "workspace": u64::MAX - 1,
            "floating": true,
            "fullscreen": false
        }]);
        let snapshot = serde_json::json!({
            "outputs": [], "workspaces": [], "windows": windows,
            "focus": {"window": u64::MAX},
            "config": {"revision": 0, "error": null},
            "theme": null, "lock": {"phase": "unlocked", "sleeping": false}
        });
        for change in [
            serde_json::json!({"type": "snapshot", "desktop": snapshot}),
            serde_json::json!({"type": "windows_changed", "windows": windows}),
        ] {
            let decoded: Change = serde_json::from_value(change.clone()).unwrap();
            assert_eq!(serde_json::to_value(&decoded).unwrap(), change);
            let decoded_windows = match decoded {
                Change::Snapshot { desktop } => desktop.windows,
                Change::WindowsChanged { windows } => windows,
                _ => unreachable!(),
            };
            assert_eq!(
                decoded_windows,
                vec![Window {
                    id: u64::MAX,
                    app_id: "org.example.Editor.desktop".into(),
                    title: "Chapter \"one\"\n第二课".into(),
                    focused: true,
                    workspace: u64::MAX - 1,
                    floating: true,
                    fullscreen: false,
                }]
            );
        }
    }

    #[test]
    fn shared_themes_preserve_the_wire_format_and_compare_by_value_after_replacement() {
        let wire = serde_json::json!({
            "outputs": [], "workspaces": [], "windows": [], "focus": null,
            "config": {"revision": 0, "error": null},
            "theme": {"mode": "dark"},
            "lock": {"phase": "unlocked", "sleeping": false}
        });
        let snapshot: Snapshot = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(&snapshot).unwrap(), wire);
        let mut next = snapshot.clone();
        assert!(Arc::ptr_eq(&snapshot.theme, &next.theme));
        assert!(next.changes_since(&snapshot).is_empty());
        next.theme = Arc::new(next.theme.as_ref().clone());
        assert!(!Arc::ptr_eq(&snapshot.theme, &next.theme));
        assert!(next.changes_since(&snapshot).is_empty());
        next.theme = serde_json::json!({"mode": "light"}).into();
        assert_eq!(
            next.changes_since(&snapshot),
            [Change::ThemeChanged {
                theme: serde_json::json!({"mode": "light"})
            }]
        );
    }

    #[test]
    fn events_round_trip_with_an_independent_schema_version() {
        let event = Event {
            version: VERSION,
            generation: 42,
            last: true,
            change: Change::FocusChanged {
                focus: serde_json::json!({"window": 7}),
            },
        };
        let mut bytes = Vec::new();
        crate::write_frame(&mut bytes, &event).unwrap();
        let decoded: Event = crate::read_frame(&mut bytes.as_slice()).unwrap();
        assert_eq!(decoded, event);
        decoded.validate().unwrap();
        assert!(
            Event {
                version: VERSION + 1,
                ..event
            }
            .validate()
            .is_err()
        );
    }
}
