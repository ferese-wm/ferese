pub mod desktop;
pub mod families;
pub mod notifications;
pub mod panel;
pub mod presets;
pub mod theme;

use std::fmt;
use std::path::PathBuf;

use kdl::{KdlDocument, KdlEntry, KdlNode, KdlValue};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

pub const DEFAULT_MATERIAL_OPACITY: f64 = 0.78;

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PanelPreset {
    #[default]
    Continuous,
    Islands,
}

pub const fn default_island_padding() -> f32 {
    4.0
}

#[derive(Debug)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(value: String) -> Self {
        Self(value)
    }
}

pub fn config_path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
        .map(|p| p.join("ferese/config.kdl"))
}

pub fn default_wallpaper() -> &'static str {
    default_wallpaper_for(theme::Appearance::Dark)
}

pub fn default_wallpaper_for(appearance: theme::Appearance) -> &'static str {
    static LIGHT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    static DARK: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let (slot, name) = match appearance {
        theme::Appearance::Light => (&LIGHT, "ferese-wallpaper-light.png"),
        theme::Appearance::Dark => (&DARK, "ferese-wallpaper-dark.jpg"),
    };
    slot.get_or_init(|| {
        let path = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(|dir| dir.join("wallpapers").join(name)))
            .filter(|path| path.is_file())
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../assets/wallpapers")
                    .join(name)
            });

        path.canonicalize().unwrap_or(path).to_string_lossy().into_owned()
    })
}

fn field(name: &str, parent: &str) -> String {
    if matches!(parent, "commands" | "custom_themes") {
        return name.to_owned();
    }

    match name {
        "binding" => "bindings".into(),
        "window-rule" => "window_rules".into(),
        "output-profile" => "output_profiles".into(),
        "output" => "outputs".into(),
        "note" => "notes".into(),
        "panel" => "panels".into(),
        "group" => "groups".into(),
        "item" => "items".into(),
        _ => name.replace('-', "_"),
    }
}

fn is_records(key: &str, parent: &str) -> bool {
    matches!(
        (parent, key),
        (
            "",
            "bindings" | "window_rules" | "output_profiles" | "autostart" | "panels"
        ) | ("output_profiles", "outputs")
            | ("desktop_widgets", "notes")
            | ("start" | "center" | "end", "groups")
            | ("groups", "items")
    )
}

fn is_array(key: &str, parent: &str) -> bool {
    parent == "commands"
        || matches!(
            key,
            "command" | "position" | "padding" | "width_presets" | "xkb_options" | "settings_command" | "outputs"
        )
}

fn scalar(value: &KdlValue) -> Result<Value, Error> {
    Ok(match value {
        KdlValue::String(s) => Value::String(s.clone()),
        KdlValue::Bool(v) => Value::Bool(*v),
        KdlValue::Null => {
            return Err(Error("Omit optional settings instead of using #null".into()));
        }
        KdlValue::Integer(n) => {
            if let Ok(n) = i64::try_from(*n) {
                n.into()
            } else if let Ok(n) = u64::try_from(*n) {
                n.into()
            } else {
                return Err(Error("Integer is outside the supported range".into()));
            }
        }
        KdlValue::Float(n) => serde_json::Number::from_f64(*n)
            .map(Value::Number)
            .ok_or_else(|| Error("Configuration numbers must be finite".into()))?,
    })
}

fn insert(map: &mut Map<String, Value>, key: String, value: Value) -> Result<(), Error> {
    if map.insert(key.clone(), value).is_some() {
        return Err(Error(format!("Duplicate setting: {key}")));
    }
    Ok(())
}

fn object(doc: &KdlDocument, parent: &str) -> Result<Map<String, Value>, Error> {
    let mut result = Map::new();

    for node in doc.nodes() {
        let key = field(node.name().value(), parent);
        let value = node_value(node, &key, parent)?;
        if is_records(&key, parent) {
            let records = result.entry(key).or_insert_with(|| Value::Array(Vec::new()));
            records.as_array_mut().unwrap().push(value);
        } else {
            insert(&mut result, key, value)?;
        }
    }

    Ok(result)
}

fn node_value(node: &KdlNode, key: &str, parent: &str) -> Result<Value, Error> {
    if node.ty().is_some() || node.entries().iter().any(|e| e.ty().is_some()) {
        return Err(Error("Type annotations are not supported in configuration".into()));
    }

    let args = node
        .entries()
        .iter()
        .filter(|e| e.name().is_none())
        .map(|e| scalar(e.value()))
        .collect::<Result<Vec<_>, _>>()?;
    let props = node
        .entries()
        .iter()
        .filter_map(|e| e.name().map(|n| (field(n.value(), key), e)))
        .collect::<Vec<_>>();

    if is_records(key, parent) {
        let mut map = node.children().map(|d| object(d, key)).transpose()?.unwrap_or_default();
        for (name, entry) in props {
            insert(&mut map, name, scalar(entry.value())?)?;
        }
        let positional: &[&str] = match key {
            "bindings" => &["keys", "action", "argument"],
            "output_profiles" => &["name"],
            "outputs" => &["match"],
            "notes" | "panels" | "groups" | "items" => &["id"],
            "autostart" => &[],
            "window_rules" => &[],
            _ => unreachable!(),
        };

        if key == "autostart" && !args.is_empty() {
            insert(&mut map, "command".into(), args.into())?;
        } else {
            if args.len() > positional.len() {
                return Err(Error(format!("Too many arguments for {}", node.name())));
            }
            for (name, value) in positional.iter().zip(args) {
                insert(&mut map, (*name).into(), value)?;
            }
        }

        return Ok(Value::Object(map));
    }

    if node.children().is_some() || !props.is_empty() {
        if !args.is_empty() {
            return Err(Error(format!(
                "{} cannot combine scalar arguments with a section",
                node.name()
            )));
        }
        let mut map = node.children().map(|d| object(d, key)).transpose()?.unwrap_or_default();
        for (name, entry) in props {
            insert(&mut map, name, scalar(entry.value())?)?;
        }
        return Ok(Value::Object(map));
    }

    if is_array(key, parent) {
        return Ok(Value::Array(args));
    }
    if args.len() != 1 {
        return Err(Error(format!("{} requires one value", node.name())));
    }

    Ok(args.into_iter().next().unwrap())
}

#[derive(Clone, Debug)]
pub struct Document {
    doc: KdlDocument,
    value: Value,
}

impl Document {
    pub fn parse(source: &str) -> Result<Self, Error> {
        let doc = source.parse::<KdlDocument>().map_err(|e| Error(format!("{e:?}")))?;
        let value = Value::Object(object(&doc, "")?);

        Ok(Self { doc, value })
    }

    pub fn value(&self) -> &Value {
        &self.value
    }

    /// A runtime view; the original KDL remains available for comment-preserving edits.
    pub fn with_theme(&self, theme: &theme::ResolvedTheme) -> Self {
        let mut document = self.clone();
        document.value["theme"] = serde_json::to_value(&theme.tokens).expect("serializable theme tokens");
        document.value["theme"]["appearance"] = serde_json::to_value(theme.appearance).unwrap();
        document
    }

    pub fn get(&self, path: &str) -> Option<&Value> {
        let mut value = &self.value;

        for part in path.split('.') {
            value = if let Ok(index) = part.parse::<usize>() {
                value.get(index)?
            } else {
                value.get(part)?
            };
        }
        Some(value)
    }

    fn refresh(&mut self) -> Result<(), Error> {
        self.value = Value::Object(object(&self.doc, "")?);
        Ok(())
    }

    pub fn set(&mut self, path: &str, value: Value) -> Result<(), Error> {
        if value.as_str() == Some("")
            && matches!(
                path,
                "theme.file" | "theme.light.file" | "theme.dark.file" | "theme.accent"
            )
        {
            return self.unset(path);
        }
        let mut candidate = self.clone();
        if matches!(
            path,
            "theme.mode"
                | "theme.family"
                | "theme.light.family"
                | "theme.dark.family"
                | "theme.light.preset"
                | "theme.dark.preset"
        ) {
            candidate.migrate_theme_selection()?;
        }
        let parts = path.split('.').collect::<Vec<_>>();
        set_in(&mut candidate.doc, &parts, "", value)?;
        format_document(&mut candidate.doc);
        candidate.refresh()?;
        *self = candidate;
        Ok(())
    }

    pub fn add(&mut self, path: &str, fields: Vec<(String, Value)>) -> Result<(), Error> {
        let mut candidate = self.clone();
        let (last, parents) = path.rsplit_once('.').map_or((path, ""), |(p, l)| (l, p));
        let parent = parents.rsplit('.').find(|s| s.parse::<usize>().is_err()).unwrap_or("");

        if !is_records(last, parent) {
            return Err(Error("Expected a list of records".into()));
        }

        let doc = section_mut(
            &mut candidate.doc,
            &parents.split('.').filter(|s| !s.is_empty()).collect::<Vec<_>>(),
            "",
        )?;

        doc.nodes_mut()
            .push(value_node(last, &Value::Object(fields.into_iter().collect()), parent)?);
        format_document(&mut candidate.doc);
        candidate.refresh()?;
        *self = candidate;

        Ok(())
    }

    fn migrate_theme_selection(&mut self) -> Result<(), Error> {
        if self.get("theme.mode").is_some() {
            return Ok(());
        }
        if self.get("theme.light").is_some() || self.get("theme.dark").is_some() {
            set_in(&mut self.doc, &["theme", "mode"], "", Value::String("dark".into()))?;
            return self.refresh();
        }
        let Some(index) = self.doc.nodes().iter().position(|node| node.name().value() == "theme") else {
            return Ok(());
        };
        if let Some(children) = self.doc.nodes_mut()[index].children_mut().as_mut() {
            let mut moved = Vec::new();
            for name in ["colors", "surface", "border", "focus-ring"] {
                if let Some(index) = children.nodes().iter().position(|node| node.name().value() == name) {
                    moved.push(children.nodes_mut().remove(index));
                }
            }
            if !moved.is_empty() {
                let dark = section_mut(children, &["dark"], "theme")?;
                dark.nodes_mut().extend(moved);
            }
        }
        set_in(&mut self.doc, &["theme", "mode"], "", Value::String("dark".into()))?;
        self.refresh()
    }

    pub fn unset(&mut self, path: &str) -> Result<(), Error> {
        if self.get(path).is_none() {
            return Ok(());
        }
        let mut candidate = self.clone();
        let (name, parents) = path.rsplit_once('.').map_or((path, ""), |(p, n)| (n, p));
        if unset_property(&mut candidate.doc, &path.split('.').collect::<Vec<_>>(), "")? {
            format_document(&mut candidate.doc);
            candidate.refresh()?;
            *self = candidate;
            return Ok(());
        }
        let doc = section_mut(
            &mut candidate.doc,
            &parents.split('.').filter(|s| !s.is_empty()).collect::<Vec<_>>(),
            "",
        )?;
        if let Some(index) = doc
            .nodes()
            .iter()
            .position(|node| field(node.name().value(), parents.rsplit('.').next().unwrap_or("")) == name)
        {
            let node = doc.nodes_mut().remove(index);
            let mut comments = String::new();
            collect_comments(&node, &mut comments);
            if let Some(format) = doc.format_mut() {
                format.trailing.push_str(&comments);
            } else {
                doc.set_format(kdl::KdlDocumentFormat {
                    leading: String::new(),
                    trailing: comments,
                });
            }
        }
        format_document(&mut candidate.doc);
        candidate.refresh()?;
        *self = candidate;
        Ok(())
    }

    pub fn remove(&mut self, path: &str, index: usize) -> Result<(), Error> {
        let mut candidate = self.clone();
        let (last, parents) = path.rsplit_once('.').map_or((path, ""), |(p, l)| (l, p));
        let doc = section_mut(
            &mut candidate.doc,
            &parents.split('.').filter(|s| !s.is_empty()).collect::<Vec<_>>(),
            "",
        )?;
        let i = doc
            .nodes()
            .iter()
            .enumerate()
            .filter(|(_, n)| field(n.name().value(), parents.rsplit('.').next().unwrap_or("")) == last)
            .nth(index)
            .map(|(i, _)| i)
            .ok_or_else(|| Error("This item no longer exists. Reload settings.".into()))?;

        doc.nodes_mut().remove(i);
        format_document(&mut candidate.doc);
        candidate.refresh()?;
        *self = candidate;

        Ok(())
    }
}

fn collect_comments(node: &KdlNode, output: &mut String) {
    let mut keep = |text: &str| {
        if text.contains("//") || text.contains("/*") {
            output.push_str(text);
            output.push('\n');
        }
    };
    if let Some(format) = node.format() {
        keep(&format.leading);
        keep(&format.before_children);
        keep(&format.before_terminator);
        keep(&format.terminator);
        keep(&format.trailing);
    }
    for entry in node.entries() {
        if let Some(format) = entry.format() {
            keep(&format.leading);
            keep(&format.trailing);
        }
    }
    if let Some(children) = node.children() {
        if let Some(format) = children.format() {
            keep(&format.leading);
            keep(&format.trailing);
        }
        for child in children.nodes() {
            collect_comments(child, output);
        }
    }
}

impl fmt::Display for Document {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.doc.fmt(f)
    }
}

fn set_entry_value(entry: &mut KdlEntry, value: KdlValue) {
    let representation = value.to_string();
    entry.set_value(value);
    if let Some(format) = entry.format_mut() {
        format.value_repr = representation;
    }
}

type NodeFormat = (Vec<Option<kdl::KdlEntryFormat>>, Option<String>);

fn collect_node_formats(doc: &KdlDocument, formats: &mut Vec<NodeFormat>) {
    for node in doc.nodes() {
        let entries = node.entries().iter().map(|entry| entry.format().cloned()).collect();
        let terminator = node
            .format()
            .map(|format| format.terminator.clone())
            .filter(|text| text.contains("//") || text.contains("/*"));
        formats.push((entries, terminator));
        if let Some(children) = node.children() {
            collect_node_formats(children, formats);
        }
    }
}

fn restore_node_formats(doc: &mut KdlDocument, formats: &mut impl Iterator<Item = NodeFormat>) {
    for node in doc.nodes_mut() {
        if let Some((entries, terminator)) = formats.next() {
            for (entry, format) in node.entries_mut().iter_mut().zip(entries) {
                if let Some(format) = format {
                    entry.set_format(format);
                }
            }
            if let Some(terminator) = terminator
                && let Some(format) = node.format_mut()
            {
                format.before_terminator = " ".into();
                format.terminator = terminator;
            }
        }
        if let Some(children) = node.children_mut() {
            restore_node_formats(children, formats);
        }
    }
}

fn format_document(doc: &mut KdlDocument) {
    let mut formats = Vec::new();
    collect_node_formats(doc, &mut formats);
    doc.autoformat();
    restore_node_formats(doc, &mut formats.into_iter());
}

fn node_index(doc: &mut KdlDocument, key: &str, parent: &str) -> usize {
    if let Some(i) = doc.nodes().iter().position(|n| field(n.name().value(), parent) == key) {
        return i;
    }

    doc.nodes_mut().push(KdlNode::new(node_name(key, parent)));
    doc.nodes().len() - 1
}

fn section_mut<'a>(doc: &'a mut KdlDocument, parts: &[&str], parent: &str) -> Result<&'a mut KdlDocument, Error> {
    if parts.is_empty() {
        return Ok(doc);
    }

    let key = parts[0];
    let i = node_index(doc, key, parent);
    let mut used = 1;
    let i = if is_records(key, parent) {
        let index = parts
            .get(1)
            .and_then(|s| s.parse::<usize>().ok())
            .ok_or_else(|| Error("Record index missing".into()))?;
        used = 2;
        doc.nodes()
            .iter()
            .enumerate()
            .filter(|(_, n)| field(n.name().value(), parent) == key)
            .nth(index)
            .map(|(i, _)| i)
            .ok_or_else(|| Error("This item no longer exists. Reload settings.".into()))?
    } else {
        i
    };
    let node = &mut doc.nodes_mut()[i];
    let child = node.children_mut().get_or_insert_with(KdlDocument::new);

    section_mut(child, &parts[used..], key)
}

/// Scalar settings in record nodes can be KDL properties rather than children.
fn unset_property(doc: &mut KdlDocument, parts: &[&str], parent: &str) -> Result<bool, Error> {
    if parts.len() < 2 {
        return Ok(false);
    }
    let key = parts[0];
    let record = is_records(key, parent);
    let used = if record { 2 } else { 1 };
    let index = if record {
        parts
            .get(1)
            .and_then(|part| part.parse::<usize>().ok())
            .ok_or_else(|| Error("Record index missing".into()))?
    } else {
        0
    };
    let Some(node) = doc
        .nodes_mut()
        .iter_mut()
        .filter(|node| field(node.name().value(), parent) == key)
        .nth(index)
    else {
        return Ok(false);
    };
    if parts.len() == used + 1
        && let Some(index) = node
            .entries()
            .iter()
            .position(|entry| entry.name().is_some_and(|name| field(name.value(), key) == parts[used]))
    {
        let entry = node.entries_mut().remove(index);
        if let Some(format) = entry.format() {
            let comments = [&format.leading, &format.trailing]
                .into_iter()
                .filter(|part| part.contains("//") || part.contains("/*"))
                .cloned()
                .collect::<String>();
            if !comments.is_empty() {
                let mut format = node.format().cloned().unwrap_or_default();
                format.leading.push_str(&comments);
                node.set_format(format);
            }
        }
        return Ok(true);
    }
    if let Some(children) = node.children_mut() {
        unset_property(children, &parts[used..], key)
    } else {
        Ok(false)
    }
}

fn set_in(doc: &mut KdlDocument, parts: &[&str], parent: &str, value: Value) -> Result<(), Error> {
    if parts.is_empty() {
        return Err(Error("Missing setting path".into()));
    }

    let key = parts[0];
    if parts.len() == 1 && is_records(key, parent) {
        let records = value
            .as_array()
            .ok_or_else(|| Error("Expected a list of records".into()))?;
        let mut nodes = records
            .iter()
            .map(|value| value_node(key, value, parent))
            .collect::<Result<Vec<_>, _>>()?;
        let at = doc
            .nodes()
            .iter()
            .position(|node| field(node.name().value(), parent) == key)
            .unwrap_or(doc.nodes().len());
        let mut comments = String::new();
        for node in doc
            .nodes()
            .iter()
            .filter(|node| field(node.name().value(), parent) == key)
        {
            collect_comments(node, &mut comments);
        }
        doc.nodes_mut().retain(|node| field(node.name().value(), parent) != key);
        if let Some(first) = nodes.first_mut() {
            first.set_format(kdl::KdlNodeFormat {
                leading: comments,
                ..Default::default()
            });
        } else if !comments.is_empty() {
            let mut format = doc.format().cloned().unwrap_or_default();
            format.trailing.push_str(&comments);
            doc.set_format(format);
        }
        doc.nodes_mut().splice(at..at, nodes);
        return Ok(());
    }
    let i = node_index(doc, key, parent);

    if parts.len() == 1 {
        let new = value_node(key, &value, parent)?;
        let node = &mut doc.nodes_mut()[i];
        if new.entries().len() == 1 && node.entries().len() == 1 && node.children().is_none() {
            set_entry_value(&mut node.entries_mut()[0], new.entries()[0].value().clone());
            node.entries_mut()[0].set_name(None::<String>);
        } else {
            *node.entries_mut() = new.entries().to_vec();
            *node.children_mut() = new.children().cloned();
        }

        return Ok(());
    }

    if is_records(key, parent) {
        let index = parts[1]
            .parse::<usize>()
            .map_err(|_| Error("Record index missing".into()))?;
        let i = doc
            .nodes()
            .iter()
            .enumerate()
            .filter(|(_, n)| field(n.name().value(), parent) == key)
            .nth(index)
            .map(|(i, _)| i)
            .ok_or_else(|| Error("This item no longer exists. Reload settings.".into()))?;
        let node = &mut doc.nodes_mut()[i];
        let rest = &parts[2..];

        if rest.len() == 1 {
            let positions: &[&str] = match key {
                "bindings" => &["keys", "action", "argument"],
                "outputs" => &["match"],
                "output_profiles" => &["name"],
                "notes" | "panels" | "groups" | "items" => &["id"],
                _ => &[],
            };
            let position = positions.iter().position(|p| *p == rest[0]);
            let mut arg_index = 0;

            for entry in node.entries_mut() {
                let matches = if let Some(name) = entry.name() {
                    field(name.value(), key) == rest[0]
                } else {
                    let matches = Some(arg_index) == position;
                    arg_index += 1;
                    matches
                };

                if matches {
                    set_entry_value(entry, kdl_value(&value)?);
                    return Ok(());
                }
            }

            if key == "autostart" && rest[0] == "command" && node.children().is_none_or(|d| d.get("command").is_none())
            {
                node.entries_mut().retain(|e| e.name().is_some());
                for value in value
                    .as_array()
                    .ok_or_else(|| Error("Command must be an array".into()))?
                {
                    node.entries_mut().push(KdlEntry::new(kdl_value(value)?));
                }
                return Ok(());
            }
        }

        return set_in(
            node.children_mut().get_or_insert_with(KdlDocument::new),
            rest,
            key,
            value,
        );
    }
    let node = &mut doc.nodes_mut()[i];

    if parts.len() == 2
        && let Some(entry) = node
            .entries_mut()
            .iter_mut()
            .find(|e| e.name().is_some_and(|n| field(n.value(), key) == parts[1]))
    {
        set_entry_value(entry, kdl_value(&value)?);
        return Ok(());
    }

    set_in(
        node.children_mut().get_or_insert_with(KdlDocument::new),
        &parts[1..],
        key,
        value,
    )
}

fn kdl_value(value: &Value) -> Result<KdlValue, Error> {
    Ok(match value {
        Value::String(s) => KdlValue::String(s.clone()),
        Value::Bool(b) => KdlValue::Bool(*b),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                KdlValue::Integer(i.into())
            } else if let Some(i) = n.as_u64() {
                KdlValue::Integer(i.into())
            } else {
                KdlValue::Float(n.as_f64().unwrap())
            }
        }
        _ => return Err(Error("Expected a scalar value".into())),
    })
}

fn node_name(key: &str, parent: &str) -> String {
    if matches!(parent, "commands" | "custom_themes") {
        return key.into();
    }

    if !is_records(key, parent) {
        return key.replace('_', "-");
    }

    match key {
        "bindings" => "binding".into(),
        "window_rules" => "window-rule".into(),
        "output_profiles" => "output-profile".into(),
        "outputs" => "output".into(),
        "notes" => "note".into(),
        "panels" => "panel".into(),
        "groups" => "group".into(),
        "items" => "item".into(),
        _ => key.replace('_', "-"),
    }
}

fn value_node(key: &str, value: &Value, parent: &str) -> Result<KdlNode, Error> {
    let mut node = KdlNode::new(node_name(key, parent));

    match value {
        Value::Object(map) => {
            let positions: &[&str] = match key {
                "bindings" => &["keys", "action", "argument"],
                "output_profiles" => &["name"],
                "outputs" => &["match"],
                "notes" | "panels" | "groups" | "items" => &["id"],
                _ => &[],
            };
            let mut child = KdlDocument::new();
            let mut consumed = Vec::new();

            for name in positions {
                if let Some(value) = map.get(*name) {
                    node.entries_mut().push(KdlEntry::new(kdl_value(value)?));
                    consumed.push(*name);
                } else {
                    break;
                }
            }

            for (name, value) in map {
                if consumed.contains(&name.as_str()) {
                    continue;
                }
                if key == "autostart" && name == "command" {
                    for value in value
                        .as_array()
                        .ok_or_else(|| Error("Command must be an array".into()))?
                    {
                        node.entries_mut().push(KdlEntry::new(kdl_value(value)?));
                    }
                } else if is_records(key, parent) && map.len() <= 4 && !value.is_array() && !value.is_object() {
                    node.entries_mut()
                        .push(KdlEntry::new_prop(name.replace('_', "-"), kdl_value(value)?));
                } else {
                    append(&mut child, name, value, key)?;
                }
            }

            if !child.nodes().is_empty() || node.entries().is_empty() {
                node.set_children(child);
            }
        }
        Value::Array(array) => {
            for value in array {
                node.entries_mut().push(KdlEntry::new(kdl_value(value)?));
            }
        }
        _ => node.entries_mut().push(KdlEntry::new(kdl_value(value)?)),
    }
    Ok(node)
}

fn append(doc: &mut KdlDocument, key: &str, value: &Value, parent: &str) -> Result<(), Error> {
    if is_records(key, parent) {
        for record in value.as_array().ok_or_else(|| Error("Expected records".into()))? {
            doc.nodes_mut().push(value_node(key, record, parent)?);
        }
    } else {
        doc.nodes_mut().push(value_node(key, value, parent)?);
    }
    Ok(())
}

pub fn from_str<T: DeserializeOwned>(source: &str) -> Result<T, Error> {
    serde_json::from_value(Document::parse(source)?.value).map_err(|e| Error(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adopting_appearance_modes_preserves_legacy_colors_and_comments() {
        let mut document = Document::parse(
            "// my desktop\ntheme {\n    colors {\n        accent \"#123456\" // my accent\n    }\n}\n",
        )
        .unwrap();
        document.set("theme.mode", serde_json::json!("light")).unwrap();
        assert_eq!(
            document.get("theme.dark.colors.accent"),
            Some(&serde_json::json!("#123456"))
        );
        assert!(document.get("theme.colors").is_none());
        assert!(document.to_string().contains("// my accent"));
        document.unset("theme.dark.colors").unwrap();
        assert!(document.to_string().contains("// my accent"));
        assert!(document.to_string().contains("// my desktop"));
        Document::parse(&document.to_string()).unwrap();
    }

    #[test]
    fn clearing_optional_theme_references_removes_them_without_empty_paths() {
        let mut document = Document::parse("theme { file \"custom_theme.kdl\"; } ").unwrap();
        document.set("theme.file", serde_json::json!("")).unwrap();
        assert!(document.get("theme.file").is_none());
        document.set("theme.file", serde_json::json!("")).unwrap();
    }

    #[test]
    fn widget_output_arrays_and_command_names_survive_edits() {
        let source = r#"commands {
    my_command "program"
}
desktop-widgets {
    clock enabled=#true {
        outputs "DP-1"
    }
    note "n" text="x" {
        outputs "DP-2"
    }
}
"#;
        let mut doc = Document::parse(source).unwrap();
        assert_eq!(doc.get("commands.my_command").unwrap(), &serde_json::json!(["program"]));
        assert_eq!(
            doc.get("desktop_widgets.clock.outputs").unwrap(),
            &serde_json::json!(["DP-1"])
        );
        assert_eq!(
            doc.get("desktop_widgets.notes.0.outputs").unwrap(),
            &serde_json::json!(["DP-2"])
        );
        doc.set("commands.new_command", serde_json::json!(["two", "words"]))
            .unwrap();
        doc.set("desktop_widgets.clock.outputs", serde_json::json!([])).unwrap();
        let reparsed = Document::parse(&doc.to_string()).unwrap();
        assert_eq!(
            reparsed.get("commands.new_command").unwrap(),
            &serde_json::json!(["two", "words"])
        );
        assert_eq!(
            reparsed.get("desktop_widgets.clock.outputs").unwrap(),
            &serde_json::json!([])
        );
    }

    #[test]
    fn inline_comments_and_failed_edits_leave_document_intact() {
        let mut doc = Document::parse("// header\nanimations {\n    speed 0.75 // keep inline\n}\n").unwrap();
        doc.set("animations.speed", 0.8.into()).unwrap();
        assert!(doc.to_string().contains("// keep inline"));
        assert_eq!(
            Document::parse(&doc.to_string())
                .unwrap()
                .get("animations.speed")
                .unwrap(),
            0.8
        );
        let previous = doc.to_string();
        assert!(doc.set("bindings.4.keys", "X".into()).is_err());
        assert_eq!(doc.to_string(), previous);
    }

    #[test]
    fn editing_autostart_commands_handles_both_representations() {
        for source in [
            "autostart \"old\" enabled=#true\n",
            "autostart { command \"old\"; enabled #true; }\n",
        ] {
            let mut doc = Document::parse(source).unwrap();
            doc.set("autostart.0.command", serde_json::json!(["new", "arg"]))
                .unwrap();
            let reparsed = Document::parse(&doc.to_string()).unwrap();
            assert_eq!(
                reparsed.get("autostart.0.command").unwrap(),
                &serde_json::json!(["new", "arg"])
            );
        }
    }

    #[test]
    fn compact_syntax_and_nested_arrays_roundtrip() {
        let source = r#"input repeat-rate=30 { xkb-options "compose:ralt"; }
binding "Swipe3Up" "toggle-overview"
output-profile "desk" { output "HDMI-A-1" scale=1.5 { position 0 0; }; }
autostart "program" "two words" enabled=#false
"#;
        let mut doc = Document::parse(source).unwrap();
        assert_eq!(doc.get("bindings.0.action").unwrap(), "toggle-overview");
        doc.set("bindings.0.action", "none".into()).unwrap();
        doc.set("input.repeat_rate", 25.into()).unwrap();
        doc.set("output_profiles.0.outputs.0.scale", 2.0.into()).unwrap();
        let reparsed = Document::parse(&doc.to_string()).unwrap();
        assert_eq!(reparsed.get("bindings.0.action").unwrap(), "none");
        assert_eq!(reparsed.get("input.repeat_rate").unwrap(), 25);
        assert_eq!(reparsed.get("output_profiles.0.outputs.0.scale").unwrap(), 2.0);
    }

    #[test]
    fn rejects_duplicate_fields_ambiguous_syntax_and_nonfinite_values() {
        for source in [
            "input { repeat-rate 25; repeat_rate 30; }",
            "binding \"X\" keys=\"Y\"",
            "input 1 { repeat-rate 25; }",
            "layout { inner-gap #inf; }",
            "layout { inner-gap #null; }",
        ] {
            assert!(Document::parse(source).is_err(), "{source}");
        }
    }

    #[test]
    fn edits_preserve_comments_commands_and_note_contents() {
        let source = r#"// personal comment
commands {
    screenshot-full "ferese-screenshot" "--full"
}
binding "Swipe3Left" "move" "left"
desktop-widgets {
    note "a" text="line one"
}
"#;
        let mut doc = Document::parse(source).unwrap();
        doc.set("desktop_widgets.notes.0.text", "one\ntwo".into()).unwrap();
        assert!(doc.to_string().contains("// personal comment"));
        let reparsed = Document::parse(&doc.to_string()).unwrap();
        assert_eq!(
            reparsed.get("commands.screenshot-full").unwrap(),
            &serde_json::json!(["ferese-screenshot", "--full"])
        );
        assert_eq!(reparsed.get("desktop_widgets.notes.0.text").unwrap(), "one\ntwo");
        doc.add("desktop_widgets.notes", vec![("id".into(), "b".into())])
            .unwrap();
        assert_eq!(doc.get("desktop_widgets.notes").unwrap().as_array().unwrap().len(), 2);
        doc.remove("desktop_widgets.notes", 0).unwrap();
        assert_eq!(doc.get("desktop_widgets.notes.0.id").unwrap(), "b");
    }
}
