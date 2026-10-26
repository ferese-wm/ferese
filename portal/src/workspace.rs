//! Portal compatibility: a workspace is an explicitly chosen, restricted view
//! of an existing monitor. Offer it only when MONITOR was requested; never
//! advertise an invented WORKSPACE bit or VIRTUAL (which creates a new monitor).
use crate::backend::Options;
use crate::capture::Source;
use crate::stream::Ready;
use serde_json::Value as Json;
use zbus::zvariant::Value;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Quality {
    Balanced,
    High,
    #[default]
    Native,
}

impl Quality {
    pub(crate) fn legacy_default() -> Self {
        Self::Balanced
    }

    pub(crate) fn wire(self) -> u32 {
        match self {
            Self::Balanced => 0,
            Self::High => 1,
            Self::Native => 2,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Balanced => "balanced",
            Self::High => "high",
            Self::Native => "native",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Balanced => "1080p",
            Self::High => "1440p",
            Self::Native => "Native",
        }
    }

    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::Balanced => "Up to 1080p, using less bandwidth and memory.",
            Self::High => "Up to 1440p for more detail.",
            Self::Native => {
                "Original display pixels for clear text. Uses more memory and bandwidth."
            }
        }
    }
}

impl std::str::FromStr for Quality {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "balanced" => Ok(Self::Balanced),
            "high" => Ok(Self::High),
            "native" => Ok(Self::Native),
            _ => Err("Workspace quality must be balanced, high, or native".into()),
        }
    }
}

pub(crate) fn sources(workspaces: &Json, outputs: &Json) -> Result<Vec<Source>, String> {
    let workspaces = workspaces.as_array().ok_or("Invalid workspace list")?;
    let outputs = outputs.as_array().ok_or("Invalid output list")?;
    Ok(workspaces
        .iter()
        .take(128)
        .filter_map(|workspace| {
            let id = workspace["id"].as_u64()?;
            let owner = workspace["output"].as_u64()?;
            let output = outputs
                .iter()
                .find(|output| output["id"].as_u64() == Some(owner) && output["enabled"] == true)?;
            let integer = |key: &str| {
                output[key]
                    .as_i64()
                    .and_then(|value| i32::try_from(value).ok())
            };
            let (width, height) = (integer("width")?, integer("height")?);
            if width <= 0 || height <= 0 {
                return None;
            }
            Some(Source {
                name: format!("workspace:{id}"),
                label: format!(
                    "Workspace {} · {}",
                    workspace["index"].as_u64()?,
                    output["name"].as_str()?
                ),
                width,
                height,
                x: integer("x")?,
                y: integer("y")?,
                scale: 1,
            })
        })
        .collect())
}

pub(crate) fn selection_unchanged(
    selected: &[Source],
    before: &Json,
    old_outputs: &Json,
    after: &Json,
    new_outputs: &Json,
) -> bool {
    selected.iter().filter_map(Source::workspace_id).all(|id| {
        let owner = |workspaces: &Json| {
            workspaces
                .as_array()?
                .iter()
                .find(|workspace| workspace["id"].as_u64() == Some(id))?["output"]
                .as_u64()
        };
        let Some(old_owner) = owner(before) else {
            return false;
        };
        if owner(after) != Some(old_owner) {
            return false;
        }
        let output = |outputs: &Json| {
            outputs
                .as_array()?
                .iter()
                .find(|output| output["id"].as_u64() == Some(old_owner))
                .cloned()
        };
        let (Some(old), Some(new)) = (output(old_outputs), output(new_outputs)) else {
            return false;
        };
        new["enabled"] == true
            && [
                "identity",
                "name",
                "width",
                "height",
                "x",
                "y",
                "scale",
                "transform",
                "current_mode",
            ]
            .into_iter()
            .all(|key| old[key] == new[key])
    })
}

pub(crate) fn metadata(source: &Source, ready: &Ready) -> Result<Options, String> {
    if source.workspace_id().is_none()
        || ready.width == 0
        || ready.height == 0
        || ready.logical_size != Some((source.width as u32, source.height as u32))
    {
        return Err("Invalid workspace stream geometry".into());
    }
    Ok(Options::from([
        ("source_type".into(), 1u32.into()),
        (
            "size".into(),
            Value::from((source.width, source.height))
                .try_to_owned()
                .map_err(|error| error.to_string())?,
        ),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::SourceKind;
    use serde_json::json;

    fn outputs() -> Json {
        json!([{"id": 5, "name": "DP-1", "enabled": true, "width": 1920, "height": 1080,
            "x": 0, "y": 0, "scale": 2.0, "identity": "drm-edid:test", "transform": "normal",
            "current_mode": {"width": 3840, "height": 2160}}])
    }

    #[test]
    fn workspace_selection_uses_identity_not_index_and_never_remembers_consent() {
        let outputs = outputs();
        let before = json!([{"id": 42, "output": 5, "index": 1}]);
        let selected = sources(&before, &outputs).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].kind(), SourceKind::Workspace(42));
        assert_eq!(selected[0].label, "Workspace 1 · DP-1");
        let renamed = json!([{"id": 42, "output": 5, "index": 3}]);
        assert!(selection_unchanged(
            &selected, &before, &outputs, &renamed, &outputs
        ));
        let replaced = json!([{"id": 43, "output": 5, "index": 1}]);
        assert!(!selection_unchanged(
            &selected, &before, &outputs, &replaced, &outputs
        ));
        let moved = json!([{"id": 42, "output": 6, "index": 1}]);
        assert!(!selection_unchanged(
            &selected, &before, &outputs, &moved, &outputs
        ));
        let mut resized = outputs.clone();
        resized[0]["current_mode"]["width"] = 2560.into();
        assert!(!selection_unchanged(
            &selected, &before, &outputs, &before, &resized
        ));
        assert!(
            crate::restore::Restore::create("org.test.App", false, &selected, &outputs, 2)
                .is_none()
        );
    }

    #[test]
    fn workspace_metadata_keeps_logical_geometry_for_a_smaller_pixel_canvas() {
        let selected = sources(&json!([{"id": 42, "output": 5, "index": 1}]), &outputs()).unwrap();
        let mut ready = Ready {
            node: 7,
            width: 1280,
            height: 720,
            logical_size: Some((1920, 1080)),
        };
        let metadata = metadata(&selected[0], &ready).unwrap();
        assert_eq!(u32::try_from(&metadata["source_type"]).unwrap(), 1);
        assert!(!metadata.contains_key("position"));
        ready.logical_size = None;
        assert!(super::metadata(&selected[0], &ready).is_err());
        for name in ["window:", "workspace:no", "workspace:18446744073709551616"] {
            assert_eq!(SourceKind::parse(name), SourceKind::Invalid);
        }
        assert_eq!(SourceKind::parse("DP-1"), SourceKind::Monitor);
    }
}
