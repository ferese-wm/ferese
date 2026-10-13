//! Compositor status and config persistence for night light, off the UI thread.
use std::sync::{Arc, mpsc};
use std::time::Duration;

use ferese_ipc::night_light::Status;
use serde_json::json;

use super::{PollState, run, wake_status};

pub(super) fn snapshot() -> Option<Status> {
    crate::compositor_ipc::call("night-light")
        .ok()
        .and_then(|value| serde_json::from_value(value).ok())
}

// Only a changed compositor status wakes the other adapters. Dropping the sender
// cancels the wait; an in-flight IPC request already has a bounded timeout.
pub(super) struct Monitor {
    _stop: mpsc::Sender<()>,
}

impl Monitor {
    pub(super) fn start(wake: Arc<PollState>) -> std::io::Result<Self> {
        let (stop, cancelled) = mpsc::channel();
        std::thread::Builder::new()
            .name("ferese-night-light-status".into())
            .spawn(move || {
                let mut previous = None;
                loop {
                    let current = snapshot();
                    if current != previous {
                        previous = current;
                        wake_status(&wake);
                    }
                    match cancelled.recv_timeout(Duration::from_secs(5)) {
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        _ => break,
                    }
                }
            })?;
        Ok(Self { _stop: stop })
    }
}

pub(super) enum Change {
    Enabled(bool),
    Temperature(u16),
}

impl Change {
    pub(super) fn save(self) -> Result<(), String> {
        let path = ferese_config::config_path().ok_or("No configuration directory")?;
        let file = ferese_config::file::ConfigFile::open(&path)?;
        let mut document = ferese_config::Document::parse(file.source()).map_err(|e| e.to_string())?;
        let (field, value) = match self {
            Self::Enabled(value) => ("night_light.enabled", json!(value)),
            Self::Temperature(value) => ("night_light.temperature", json!(value)),
        };
        document.set(field, value).map_err(|e| e.to_string())?;
        let settings: ferese_config::night_light::Settings =
            serde_json::from_value(document.value()["night_light"].clone()).map_err(|e| e.to_string())?;
        settings.validate()?;
        file.replace(&document.to_string(), None, |staged| {
            run(
                "ferese",
                &["--check-config", staged.to_str().ok_or("Invalid configuration path")?],
            )
            .map(|_| ())
        })?;
        crate::compositor_ipc::call("reload-config").map(|_| ())
    }
}
