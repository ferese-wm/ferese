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

/// A coherent logical desktop view. Desktop objects are redacted while locked.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Snapshot {
    pub outputs: Value,
    pub workspaces: Value,
    pub windows: Value,
    pub focus: Value,
    pub config: ConfigState,
    pub theme: Value,
    pub lock: LockState,
}

/// Replacement values for each changed domain; not animation frames or client damage.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Change {
    Snapshot { desktop: Snapshot },
    OutputsChanged { outputs: Value },
    WorkspacesChanged { workspaces: Value },
    WindowsChanged { windows: Value },
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
    pub fn changes_since(&self, previous: &Self) -> Vec<Change> {
        let mut changes = Vec::new();
        if self.outputs != previous.outputs {
            changes.push(Change::OutputsChanged {
                outputs: self.outputs.clone(),
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
        if self.theme != previous.theme {
            changes.push(Change::ThemeChanged {
                theme: self.theme.clone(),
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
