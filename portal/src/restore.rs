use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use zbus::zvariant::{OwnedValue, Value};

use crate::backend::Options;
use crate::capture::Source;

const VENDOR: &str = "Ferese";
const VERSION: u32 = 1;
const MAX_DATA: usize = 16 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Restore {
    app: String,
    cursor: bool,
    mode: u32,
    monitors: Vec<Monitor>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Monitor {
    name: String,
    identity: String,
}

pub(crate) fn persist_mode(options: &Options) -> zbus::fdo::Result<u32> {
    let mode = options
        .get("persist_mode")
        .map(u32::try_from)
        .transpose()
        .map_err(|_| zbus::fdo::Error::InvalidArgs("Invalid persistence option".into()))?
        .unwrap_or(0);
    if mode > 2 {
        return Err(zbus::fdo::Error::InvalidArgs(
            "Unsupported persistence mode".into(),
        ));
    }
    Ok(mode)
}

fn identity<'a>(outputs: &'a Json, name: &str) -> Option<&'a str> {
    outputs.as_array()?.iter().find_map(|output| {
        (output["name"].as_str() == Some(name) && output["enabled"] == true)
            .then(|| output["identity"].as_str())
            .flatten()
            .filter(|identity| {
                !identity.is_empty() && identity.len() <= 512 && identity.starts_with("drm-edid:")
            })
    })
}

pub(crate) fn selection_unchanged(sources: &[Source], before: &Json, after: &Json) -> bool {
    let find = |outputs: &Json, name: &str| {
        outputs
            .as_array()
            .and_then(|outputs| {
                outputs.iter().find(|output| {
                    output["name"].as_str() == Some(name) && output["enabled"] == true
                })
            })
            .map(|output| (output["identity"].clone(), output["id"].clone()))
    };
    sources.iter().all(
        |source| match (find(before, &source.name), find(after, &source.name)) {
            (Some((identity, id)), Some((current_identity, current_id))) => {
                if identity.is_null() {
                    id.as_u64().is_some() && id == current_id && current_identity.is_null()
                } else {
                    identity == current_identity
                }
            }
            _ => false,
        },
    )
}

impl Restore {
    pub(crate) fn read(options: &Options) -> Option<Self> {
        let value = options.get("restore_data")?.try_clone().ok()?;
        let (vendor, version, data): (String, u32, OwnedValue) = value.try_into().ok()?;
        if vendor != VENDOR || version != VERSION {
            return None;
        }
        let data = data.downcast_ref::<&str>().ok()?;
        if data.len() > MAX_DATA {
            return None;
        }
        let restore: Self = serde_json::from_str(data).ok()?;
        if restore.app.is_empty()
            || restore.app.len() > 512
            || !(1..=2).contains(&restore.mode)
            || restore.monitors.is_empty()
            || restore.monitors.len() > 16
            || restore.monitors.iter().any(|monitor| {
                monitor.name.is_empty()
                    || monitor.name.len() > 512
                    || monitor.identity.is_empty()
                    || monitor.identity.len() > 512
            })
        {
            return None;
        }
        Some(restore)
    }

    pub(crate) fn effective_mode(&self, requested: u32) -> u32 {
        self.mode.min(requested)
    }

    pub(crate) fn resolve(
        &self,
        app: &str,
        cursor: bool,
        multiple: bool,
        sources: &[Source],
        outputs: &Json,
    ) -> Option<Vec<Source>> {
        if self.app != app || self.cursor != cursor || (!multiple && self.monitors.len() != 1) {
            return None;
        }
        let mut selected: Vec<Source> = Vec::new();
        for monitor in &self.monitors {
            if identity(outputs, &monitor.name) != Some(monitor.identity.as_str())
                || selected.iter().any(|source| source.name == monitor.name)
            {
                return None;
            }
            selected.push(
                sources
                    .iter()
                    .find(|source| source.name == monitor.name)?
                    .clone(),
            );
        }
        Some(selected)
    }

    pub(crate) fn create(
        app: &str,
        cursor: bool,
        sources: &[Source],
        outputs: &Json,
        mode: u32,
    ) -> Option<Self> {
        if app.is_empty()
            || app.len() > 512
            || sources.is_empty()
            || sources.len() > 16
            || sources.iter().any(|source| !source.is_monitor())
            || !(1..=2).contains(&mode)
        {
            return None;
        }
        let monitors = sources
            .iter()
            .map(|source| {
                Some(Monitor {
                    name: source.name.clone(),
                    identity: identity(outputs, &source.name)?.to_owned(),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            app: app.into(),
            cursor,
            mode,
            monitors,
        })
    }

    pub(crate) fn value(&self) -> Result<OwnedValue, String> {
        let data = serde_json::to_string(self).map_err(|error| error.to_string())?;
        if data.len() > MAX_DATA {
            return Err("Saved selection is too large".into());
        }
        Value::from((VENDOR.to_owned(), VERSION, Value::new(data)))
            .try_to_owned()
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source() -> Source {
        Source {
            name: "DP-1".into(),
            label: "Display".into(),
            width: 1920,
            height: 1080,
            x: 0,
            y: 0,
            scale: 1,
        }
    }

    #[test]
    fn restore_is_bound_to_app_capture_options_and_display_identity() {
        let outputs =
            serde_json::json!([{"name":"DP-1", "identity":"drm-edid:123", "enabled":true}]);
        let sources = [source()];
        let saved = Restore::create("org.test.App", true, &sources, &outputs, 2).unwrap();
        let options = Options::from([("restore_data".into(), saved.value().unwrap())]);
        let restored = Restore::read(&options).unwrap();
        assert!(
            restored
                .resolve("org.test.App", true, false, &sources, &outputs)
                .is_some()
        );
        assert!(
            restored
                .resolve("org.other.App", true, false, &sources, &outputs)
                .is_none()
        );
        assert!(
            restored
                .resolve("org.test.App", false, false, &sources, &outputs)
                .is_none()
        );
        assert!(
            restored
                .resolve("org.test.App", true, false, &[], &outputs)
                .is_none()
        );
        let changed =
            serde_json::json!([{"name":"DP-1", "identity":"drm-edid:456", "enabled":true}]);
        assert!(
            restored
                .resolve("org.test.App", true, false, &sources, &changed)
                .is_none()
        );
        let disabled =
            serde_json::json!([{"name":"DP-1", "identity":"drm-edid:123", "enabled":false}]);
        assert!(
            restored
                .resolve("org.test.App", true, false, &sources, &disabled)
                .is_none()
        );
    }

    #[test]
    fn transient_permission_cannot_be_upgraded_without_new_consent() {
        let outputs =
            serde_json::json!([{"name":"DP-1", "identity":"drm-edid:123", "enabled":true}]);
        let saved = Restore::create("org.test.App", false, &[source()], &outputs, 1).unwrap();
        let restored = Restore::read(&Options::from([(
            "restore_data".into(),
            saved.value().unwrap(),
        )]))
        .unwrap();
        assert_eq!(restored.effective_mode(2), 1);
        assert_eq!(restored.effective_mode(0), 0);
        let weak = serde_json::json!([{"name":"DP-1", "identity":"drm:DP-1", "enabled":true}]);
        assert!(Restore::create("org.test.App", false, &[source()], &weak, 2).is_none());
        assert!(!selection_unchanged(&[source()], &outputs, &weak));
    }

    #[test]
    fn anonymous_nested_selection_has_lifetime_checks_without_saved_grants() {
        let outputs = serde_json::json!([{"name":"DP-1", "id":1, "identity":null, "enabled":true}]);
        assert!(selection_unchanged(&[source()], &outputs, &outputs));
        let replacement =
            serde_json::json!([{"name":"DP-1", "id":2, "identity":null, "enabled":true}]);
        assert!(!selection_unchanged(&[source()], &outputs, &replacement));
        assert!(Restore::create("org.test.App", false, &[source()], &outputs, 2).is_none());
    }

    #[test]
    fn restored_multiple_selection_rejects_duplicates_and_single_source_requests() {
        let mut second = source();
        second.name = "DP-2".into();
        let sources = [source(), second];
        let outputs = serde_json::json!([
            {"name":"DP-1", "identity":"drm-edid:123", "enabled":true},
            {"name":"DP-2", "identity":"drm-edid:456", "enabled":true}
        ]);
        let mut saved = Restore::create("org.test.App", false, &sources, &outputs, 2).unwrap();
        assert_eq!(
            saved
                .resolve("org.test.App", false, true, &sources, &outputs)
                .unwrap()
                .len(),
            2
        );
        assert!(
            saved
                .resolve("org.test.App", false, false, &sources, &outputs)
                .is_none()
        );
        saved.monitors[1] = saved.monitors[0].clone();
        assert!(
            saved
                .resolve("org.test.App", false, true, &sources, &outputs)
                .is_none()
        );
    }

    #[test]
    fn foreign_malformed_and_unbounded_restore_data_are_ignored() {
        for data in ["garbage".to_owned(), "x".repeat(MAX_DATA + 1)] {
            let value = Value::from((VENDOR.to_owned(), VERSION, Value::new(data)))
                .try_to_owned()
                .unwrap();
            assert!(Restore::read(&Options::from([("restore_data".into(), value)])).is_none());
        }
        let foreign = Value::from(("GNOME".to_owned(), VERSION, Value::new("{}")))
            .try_to_owned()
            .unwrap();
        assert!(Restore::read(&Options::from([("restore_data".into(), foreign)])).is_none());
        assert!(Restore::read(&Options::new()).is_none());
        assert!(persist_mode(&Options::from([("persist_mode".into(), 3u32.into())])).is_err());
        assert!(Restore::create("", false, &[source()], &serde_json::json!([]), 2).is_none());
    }
}
