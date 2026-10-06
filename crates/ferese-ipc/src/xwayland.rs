use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const STATUS_COMMAND: &str = "xwayland-status";
pub const RETRY_COMMAND: &str = "xwayland-retry";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Disabled,
    Idle,
    Starting,
    Running,
    Backoff,
    Failed,
    Stopped,
}

impl State {
    pub fn is_inactive(self) -> bool {
        matches!(self, Self::Disabled | Self::Idle | Self::Stopped)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    Pending,
    Verified,
    Rejected,
    NotAttempted,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Status {
    pub enabled: bool,
    pub effective_startup: String,
    pub state: State,
    pub display: Option<String>,
    /// PID of Satellite, not its Xwayland child.
    pub satellite_pid: Option<u32>,
    pub generation: Option<u64>,
    pub readiness: Readiness,
    pub recent_failures: u32,
    pub restart_required: bool,
    pub last_error: Option<String>,
}

impl Status {
    pub fn from_value(value: Value) -> serde_json::Result<Self> {
        serde_json::from_value(value)
    }

    pub fn to_value(&self) -> Value {
        match serde_json::to_value(self) {
            Ok(value) => value,
            Err(error) => serde_json::json!({ "error": error.to_string() }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn states_use_stable_snake_case_names() {
        for (state, name) in [
            (State::Disabled, "disabled"),
            (State::Idle, "idle"),
            (State::Starting, "starting"),
            (State::Running, "running"),
            (State::Backoff, "backoff"),
            (State::Failed, "failed"),
            (State::Stopped, "stopped"),
        ] {
            assert_eq!(serde_json::to_value(state).unwrap(), json!(name));
        }
    }

    #[test]
    fn readiness_values_round_trip() {
        for (readiness, name) in [
            (Readiness::Pending, "pending"),
            (Readiness::Verified, "verified"),
            (Readiness::Rejected, "rejected"),
            (Readiness::NotAttempted, "not_attempted"),
        ] {
            let encoded = serde_json::to_value(readiness).unwrap();
            assert_eq!(encoded, json!(name));
            assert_eq!(serde_json::from_value::<Readiness>(encoded).unwrap(), readiness);
        }
    }

    #[test]
    fn a_status_snapshot_never_carries_credentials() {
        let status = Status {
            enabled: true,
            effective_startup: "on-demand".to_owned(),
            state: State::Running,
            display: Some(":7".to_owned()),
            satellite_pid: Some(24017),
            generation: Some(2),
            readiness: Readiness::Verified,
            recent_failures: 1,
            restart_required: false,
            last_error: None,
        };
        let value = status.to_value();
        let text = value.to_string();
        assert!(!text.contains("MIT-MAGIC-COOKIE-1"));
        assert_eq!(value["satellite_pid"], json!(24017));
        assert_eq!(value["display"], json!(":7"));
        assert_eq!(value["state"], json!("running"));
        assert_eq!(Status::from_value(value).unwrap(), status);
    }

    #[test]
    fn an_unknown_field_from_a_newer_compositor_is_ignored() {
        let decoded = Status::from_value(json!({
            "enabled": true,
            "effective_startup": "eager",
            "state": "running",
            "display": ":7",
            "satellite_pid": 1,
            "generation": 1,
            "readiness": "verified",
            "recent_failures": 0,
            "restart_required": false,
            "last_error": null,
            "future_field": {"anything": true},
        }))
        .expect("a newer field must not break decoding");

        assert_eq!(decoded.state, State::Running);
        assert_eq!(decoded.effective_startup, "eager");
    }
}
