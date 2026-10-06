//! Shared IPC types for the compositor-managed X11 service.
//!
//! Status is deliberately narrow: it never carries the X11 cookie, the
//! authority file's contents, or a raw Xwayland PID. The Xwayland PID is
//! distinct from the Satellite PID, and reporting the latter under the former's
//! name would be a lie a user could act on.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Command names understood by the compositor.
pub const STATUS_COMMAND: &str = "xwayland-status";
pub const RETRY_COMMAND: &str = "xwayland-retry";

/// Lifecycle phase of the managed service, stable across versions.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// X11 is switched off in the configuration.
    Disabled,
    /// Reserved and ready, but no process started yet (on-demand).
    Idle,
    /// A process is starting and readiness has not been verified.
    Starting,
    /// Readiness was verified for the current generation.
    Running,
    /// A bounded retry delay is in effect after a failure.
    Backoff,
    /// Fail-fast: retries are exhausted until an explicit retry.
    Failed,
    /// Intentional shutdown.
    Stopped,
}

impl State {
    /// True when no X11 process exists and none is being started.
    pub fn is_inactive(self) -> bool {
        matches!(self, Self::Disabled | Self::Idle | Self::Stopped)
    }
}

/// Readiness verification result for the current generation.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    /// No notification has been received yet.
    Pending,
    /// An exact `READY=1` from the service's own PID was accepted.
    Verified,
    /// A notification was seen but rejected (wrong PID or malformed).
    Rejected,
    /// Verification never completed for this generation.
    NotAttempted,
}

/// Snapshot returned by `xwayland-status`.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Status {
    pub enabled: bool,
    /// The configured startup policy, as an adjective for display.
    pub effective_startup: String,
    pub state: State,
    /// The reserved display, or `None` when X11 is unavailable.
    pub display: Option<String>,
    /// The Satellite PID, which Ferese does own.
    pub satellite_pid: Option<u32>,
    pub generation: Option<u64>,
    pub readiness: Readiness,
    pub recent_failures: u32,
    /// True when the configuration changed and a new session is required.
    pub restart_required: bool,
    /// Why X11 is unavailable, when it is. Never contains credentials.
    pub last_error: Option<String>,
}

impl Status {
    /// Decode a status payload, tolerating unknown fields from a newer compositor.
    pub fn from_value(value: Value) -> serde_json::Result<Self> {
        serde_json::from_value(value)
    }

    pub fn to_value(&self) -> Value {
        match serde_json::to_value(self) {
            Ok(value) => value,
            // A snapshot of owned, bounded strings cannot realistically fail.
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
        // The cookie lives in the authority file and must never be exported.
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
