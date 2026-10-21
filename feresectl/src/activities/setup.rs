use ferese_config::{Document, file::ConfigFile, panel};
use serde_json::json;
use std::{error::Error, path::Path};

fn prepare(source: &str) -> Result<String, Box<dyn Error>> {
    let mut doc = Document::parse(source)?;
    if !ferese_config::activities::setup_shell_hook(&mut doc)? {
        return Ok(source.into());
    }
    if doc.get("panels").is_none() {
        doc.set(
            "panels",
            serde_json::to_value(vec![panel::Panel::default()])?,
        )?;
    }
    let panels: Vec<panel::Panel> = serde_json::from_value(doc.get("panels").unwrap().clone())?;
    panel::validate(&panels)?;
    for (index, bar) in panels.iter().enumerate() {
        let groups = [&bar.start, &bar.center, &bar.end]
            .into_iter()
            .flat_map(|zone| &zone.groups)
            .collect::<Vec<_>>();
        if groups
            .iter()
            .flat_map(|group| &group.items)
            .any(|item| item.kind == panel::ItemKind::Activities)
        {
            continue;
        }
        let mut id = "activities".to_string();
        while groups
            .iter()
            .flat_map(|group| &group.items)
            .any(|item| item.id.0 == id)
        {
            id.push('-');
        }
        let item = serde_json::to_value(panel::Item::new(&id, panel::ItemKind::Activities))?;
        // Append within an existing group, preserving its position and every other item.
        let zone = ["end", "center", "start"].into_iter().find(|zone| {
            doc.get(&format!("panels.{index}.{zone}.groups"))
                .and_then(serde_json::Value::as_array)
                .is_some_and(|groups| !groups.is_empty())
        });
        if let Some(zone) = zone {
            doc.add(
                &format!("panels.{index}.{zone}.groups.0.items"),
                item.as_object().unwrap().clone().into_iter().collect(),
            )?;
        } else {
            doc.add(
                &format!("panels.{index}.end.groups"),
                vec![
                    ("id".into(), json!("activities")),
                    ("items".into(), json!([item])),
                ],
            )?;
        }
    }
    let panels = serde_json::from_value::<Vec<panel::Panel>>(doc.get("panels").unwrap().clone())?;
    panel::validate(&panels)?;
    Ok(doc.to_string())
}

pub fn configure(path: &Path) -> Result<(), Box<dyn Error>> {
    let file = ConfigFile::open(path)?;
    let desired = prepare(file.source())?;
    if desired == file.source() {
        return Ok(());
    }
    let backup = path.with_file_name(format!(
        "{}.before-activities-{}",
        path.file_name().unwrap().to_string_lossy(),
        ferese_ipc::activities::now_ms()
    ));
    file.replace(&desired, Some(&backup), |_| Ok(()))?;
    println!(
        "Configured Live Activities; previous config: {}",
        backup.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn setup_preserves_layout_comments_rules_and_explicit_disable() {
        let source = "// keep my layout\npanel main { style morph; end { group tools { item activities kind=clock; }; }; }\n";
        let next = prepare(source).unwrap();
        assert!(next.contains("// keep my layout"));
        let doc = Document::parse(&next).unwrap();
        assert_eq!(doc.get("panels.0.style"), Some(&json!("morph")));
        assert_eq!(
            doc.get("panels.0.end.groups.0.items.0.kind"),
            Some(&json!("clock"))
        );
        assert_eq!(
            doc.get("panels.0.end.groups.0.items.1.id"),
            Some(&json!("activities-"))
        );
        assert_eq!(prepare(&next).unwrap(), next);
        let disabled = "activities { sources { shell-hook #false; }; }\n";
        assert_eq!(prepare(disabled).unwrap(), disabled);
        let hidden =
            "panel main { end { group tools { item tasks kind=activities visible=#false; }; }; }\n";
        let hidden = Document::parse(&prepare(hidden).unwrap()).unwrap();
        assert_eq!(
            hidden.get("panels.0.end.groups.0.items.0.visible"),
            Some(&json!(false))
        );
        assert_eq!(
            hidden
                .get("panels.0.end.groups.0.items")
                .unwrap()
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(prepare("").is_ok());
        let partial =
            Document::parse(&prepare("activities { max-visible 1; }\n").unwrap()).unwrap();
        assert_eq!(partial.get("activities.max_visible"), Some(&json!(1)));
        assert_eq!(
            partial.get("activities.sources.shell_hook"),
            Some(&json!(true))
        );
        assert!(prepare("activities { unknown 1; }").is_err());
    }

    #[test]
    fn configuration_is_backed_up_and_invalid_input_is_not_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.kdl");
        std::fs::write(&path, "// original\n").unwrap();
        configure(&path).unwrap();
        configure(&path).unwrap();
        let backups: Vec<_> = std::fs::read_dir(directory.path())
            .unwrap()
            .flatten()
            .filter(|file| {
                file.file_name()
                    .to_string_lossy()
                    .contains(".before-activities-")
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(
            std::fs::read_to_string(backups[0].path()).unwrap(),
            "// original\n"
        );
        std::fs::write(&path, "activities { bad #true; }\n").unwrap();
        assert!(configure(&path).is_err());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "activities { bad #true; }\n"
        );
    }
}
