//! Activity rules are matched in declaration order, followed by unreplaced
//! built-ins. Matching metadata is bounded and never stored in the result.
use crate::document;
use kdl::KdlNode;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::Error;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub max_visible: usize,
    pub show_badge: bool,
    pub max_total: usize,
    pub owner_ceiling: u8,
    pub start_delay_ms: u64,
    pub result_flash_ms: u64,
    pub sources: Sources,
    pub pomodoro: ferese_activities::pomodoro::Settings,
    pub rules: Vec<Rule>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            max_visible: 2,
            show_badge: true,
            max_total: 128,
            owner_ceiling: 160,
            start_delay_ms: 300,
            result_flash_ms: 1_500,
            sources: Sources::default(),
            pomodoro: Default::default(),
            rules: builtins(),
        }
    }
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sources {
    pub shell_hook: bool,
    pub process_watch: bool,
    pub notification_bridge: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub name: String,
    #[serde(rename = "match")]
    pub pattern: String,
    pub title: String,
    pub icon: String,
    pub priority: u8,
    pub progress: String,
    pub min_duration_ms: u64,
    pub ignore_parents: Vec<String>,
    pub dedupe_by: Option<String>,
    pub flash_on_fail: bool,
    pub result: ResultText,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResultText {
    pub ok: String,
    pub fail: String,
}

#[derive(Clone, Debug)]
pub struct Matcher {
    rules: Vec<(Rule, Regex)>,
}
#[derive(Clone, Debug)]
pub struct Matched {
    pub rule: Rule,
    pub title: String,
    pub group: Option<String>,
}
impl Matcher {
    pub fn new(settings: &Settings) -> Result<Self, String> {
        let mut rules = Vec::new();
        for rule in &settings.rules {
            let regex = regex::RegexBuilder::new(&rule.pattern)
                .size_limit(256 * 1024)
                .dfa_size_limit(256 * 1024)
                .build()
                .map_err(|error| error.to_string())?;
            validate_rule(rule, &regex)?;
            rules.push((rule.clone(), regex));
        }
        Ok(Self { rules })
    }
    pub fn matches(&self, command: &str, cwd: &str, parents: &[String]) -> Option<Matched> {
        if command.len() > 4_096 || cwd.len() > 1_024 || parents.len() > 16 {
            return None;
        }
        for (rule, regex) in &self.rules {
            if parents
                .iter()
                .any(|parent| rule.ignore_parents.contains(parent))
            {
                continue;
            }
            if let Some(captures) = regex.captures(command) {
                let mut title = rule.title.clone();
                for index in 0..captures.len() {
                    title = title.replace(
                        &format!("{{{index}}}"),
                        captures.get(index).map_or("", |m| m.as_str()),
                    );
                }
                if title.len() > 256 || title.chars().any(char::is_control) {
                    return None;
                }
                return Some(Matched {
                    rule: rule.clone(),
                    title,
                    group: (rule.dedupe_by.as_deref() == Some("cwd")).then(|| {
                        use std::hash::{Hash, Hasher};
                        let mut hash = std::collections::hash_map::DefaultHasher::new();
                        cwd.hash(&mut hash);
                        format!("{}:{:x}", rule.name, hash.finish())
                    }),
                });
            }
        }
        None
    }
}
fn validate_rule(rule: &Rule, regex: &Regex) -> Result<(), String> {
    for value in [&rule.name, &rule.title, &rule.result.ok, &rule.result.fail] {
        if value.len() > 256 || value.chars().any(char::is_control) {
            return Err("rule text exceeds its budget".into());
        }
    }
    if rule.pattern.len() > 2_048
        || rule.min_duration_ms > 60_000
        || !matches!(rule.progress.as_str(), "indeterminate" | "cargo-json")
        || rule
            .dedupe_by
            .as_deref()
            .is_some_and(|value| value != "cwd")
    {
        return Err("invalid rule duration, progress adapter or dedupe policy".into());
    }
    if rule.ignore_parents.len() > 16 || rule.ignore_parents.iter().any(|name| name.len() > 128) {
        return Err("too many or too long parent names".into());
    }
    if rule.icon.len() > 64
        || !rule
            .icon
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err("invalid symbolic icon".into());
    }
    let capture = Regex::new(r"\{([^}]+)\}").expect("static pattern");
    for reference in capture.captures_iter(&rule.title) {
        if reference[1]
            .parse::<usize>()
            .ok()
            .is_none_or(|index| index >= regex.captures_len())
        {
            return Err("invalid title capture reference".into());
        }
    }
    Ok(())
}
impl Settings {
    pub fn limits(&self) -> ferese_activities::Limits {
        ferese_activities::Limits {
            max_visible: self.max_visible,
            show_badge: self.show_badge,
            max_total: self.max_total,
            owner_ceiling: self.owner_ceiling,
            start_delay_ms: self.start_delay_ms,
            result_flash_ms: self.result_flash_ms,
        }
    }
    pub fn validate(&self) -> Result<(), String> {
        self.pomodoro.validate().map_err(str::to_owned)?;
        if !(1..=2).contains(&self.max_visible)
            || !(1..=128).contains(&self.max_total)
            || self.start_delay_ms > 60_000
            || self.result_flash_ms > 10_000
            || self.rules.len() > 64
        {
            return Err("activity settings exceed supported limits".into());
        }
        let mut names = std::collections::HashSet::new();
        if self.rules.iter().any(|rule| !names.insert(&rule.name)) {
            return Err("duplicate activity rule".into());
        }
        Matcher::new(self).map(|_| ())
    }
}

fn diagnostic(node: &KdlNode, message: impl std::fmt::Display) -> Error {
    Error::from(format!(
        "activities at byte {}: {message}",
        node.span().offset()
    ))
}

pub(crate) fn parse(node: &KdlNode) -> Result<Value, Error> {
    if !node.entries().is_empty() {
        return Err(diagnostic(node, "section takes no properties"));
    }
    let mut values = Map::new();
    let mut rules = Vec::new();
    let mut names = std::collections::HashSet::new();
    for child in node.children().into_iter().flat_map(|doc| doc.nodes()) {
        let key = child.name().value();
        if key == "rule" {
            let Some(name) = child.get(0).and_then(kdl::KdlValue::as_string) else {
                return Err(diagnostic(child, "rule requires a name"));
            };
            if child.entries().len() != 1 || !names.insert(name.to_owned()) {
                return Err(diagnostic(child, "duplicate rule or extra rule properties"));
            }
            let mut value = Map::new();
            value.insert("name".into(), json!(name));
            for property in child.children().into_iter().flat_map(|doc| doc.nodes()) {
                let name = property.name().value();
                if !matches!(
                    name,
                    "match"
                        | "title"
                        | "icon"
                        | "priority"
                        | "progress"
                        | "min-duration-ms"
                        | "ignore-parents"
                        | "dedupe-by"
                        | "flash-on-fail"
                        | "result"
                ) {
                    return Err(diagnostic(property, "unknown rule node"));
                }
                let key = name.replace('-', "_");
                let parsed = if name == "ignore-parents" {
                    if property
                        .entries()
                        .iter()
                        .any(|entry| entry.name().is_some())
                        || property.entries().is_empty()
                    {
                        return Err(diagnostic(property, "ignore-parents requires names"));
                    }
                    Value::Array(
                        property
                            .entries()
                            .iter()
                            .map(|entry| document::scalar(entry.value()))
                            .collect::<Result<_, _>>()?,
                    )
                } else {
                    document::node_value(property, &key, "rule")
                        .map_err(|error| diagnostic(property, error))?
                };
                if value.insert(key, parsed).is_some() {
                    return Err(diagnostic(property, "duplicate rule setting"));
                }
            }
            value.entry("icon").or_insert(json!("ferese"));
            value.entry("priority").or_insert(json!(100));
            value.entry("progress").or_insert(json!("indeterminate"));
            value.entry("min_duration_ms").or_insert(json!(0));
            value.entry("ignore_parents").or_insert(json!([]));
            value.entry("dedupe_by").or_insert(Value::Null);
            value.entry("flash_on_fail").or_insert(json!(true));
            value
                .entry("result")
                .or_insert(json!({"ok":"Finished","fail":"Command failed"}));
            let rule: Rule = serde_json::from_value(Value::Object(value))
                .map_err(|error| diagnostic(child, error))?;
            let settings = Settings {
                rules: vec![rule.clone()],
                ..Default::default()
            };
            settings
                .validate()
                .map_err(|error| diagnostic(child, error))?;
            rules.push(rule);
        } else {
            if !matches!(
                key,
                "max-visible"
                    | "show-badge"
                    | "max-total"
                    | "owner-ceiling"
                    | "start-delay-ms"
                    | "result-flash-ms"
                    | "sources"
                    | "pomodoro"
            ) {
                return Err(diagnostic(child, "unknown activity node"));
            }
            let field = key.replace('-', "_");
            let value = document::node_value(child, &field, "activities")
                .map_err(|error| diagnostic(child, error))?;
            if values.insert(field, value).is_some() {
                return Err(diagnostic(child, "duplicate activity setting"));
            }
        }
    }
    rules.extend(
        builtins()
            .into_iter()
            .filter(|rule| !names.contains(&rule.name)),
    );
    values.insert("rules".into(), serde_json::to_value(rules).unwrap());
    let settings: Settings =
        serde_json::from_value(Value::Object(values)).map_err(|error| diagnostic(node, error))?;
    settings
        .validate()
        .map_err(|error| diagnostic(node, error))?;
    Ok(serde_json::to_value(settings).unwrap())
}
fn builtins() -> Vec<Rule> {
    [
        (
            "cargo",
            r"^cargo (build|check|test|run|clippy)\b",
            "cargo {1}",
            "Finished",
            1_500,
        ),
        (
            "npm",
            r"^(npm|pnpm) (install|build|test|run)\b",
            "{1} {2}",
            "Finished",
            1_500,
        ),
        ("make", r"^make(?:\s|$)", "Make", "Built", 300),
        ("git-push", r"^git push(?:\s|$)", "Git push", "Pushed", 300),
        (
            "docker-build",
            r"^docker build(?:\s|$)",
            "Docker build",
            "Image built",
            1_500,
        ),
    ]
    .into_iter()
    .map(|(name, pattern, title, ok, min_duration_ms)| Rule {
        name: name.into(),
        pattern: pattern.into(),
        title: title.into(),
        icon: "hammer".into(),
        priority: 100,
        progress: "indeterminate".into(),
        min_duration_ms,
        ignore_parents: if name == "cargo" {
            vec!["rust-analyzer".into()]
        } else {
            vec![]
        },
        dedupe_by: Some("cwd".into()),
        flash_on_fail: true,
        result: ResultText {
            ok: ok.into(),
            fail: "Command failed".into(),
        },
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pomodoro_settings_are_strict_and_validate_duration_ranges() {
        let doc=crate::Document::parse("activities { pomodoro { work-minutes 40; short-break-minutes 7; long-break-minutes 20; intervals 3; auto-advance #true; notifications #false; }; }").unwrap();
        let settings: Settings = serde_json::from_value(doc.value()["activities"].clone()).unwrap();
        assert_eq!(settings.pomodoro.work_minutes, 40);
        assert_eq!(settings.pomodoro.intervals, 3);
        assert!(settings.pomodoro.auto_advance);
        assert!(!settings.pomodoro.notifications);
        for invalid in [
            "work-minutes 0",
            "short-break-minutes 61",
            "long-break-minutes 121",
            "intervals 13",
            "work-minuts 20",
        ] {
            assert!(
                crate::Document::parse(&format!("activities {{ pomodoro {{ {invalid}; }}; }}"))
                    .is_err(),
                "{invalid}"
            );
        }
    }
    #[test]
    fn settings_are_opt_in_and_matching_discards_nonmatches() {
        let settings = Settings::default();
        assert!(!settings.sources.shell_hook);
        assert!(!settings.sources.process_watch);
        let matcher = Matcher::new(&settings).unwrap();
        assert!(matcher.matches("secret command", "/tmp", &[]).is_none());
        let matched = matcher.matches("cargo test", "/tmp", &[]).unwrap();
        assert_eq!(matched.title, "cargo test");
        assert!(
            matcher
                .matches("cargo test", "/tmp", &["rust-analyzer".into()])
                .is_none()
        );
    }
    #[test]
    fn shipped_kdl_matches_the_builtin_catalog() {
        let document = crate::Document::parse(include_str!("../activities.kdl")).unwrap();
        assert_eq!(
            document.value()["activities"]["rules"],
            serde_json::to_value(Settings::default().rules).unwrap()
        );
    }
    #[test]
    fn complete_rule_replacement_and_diagnostics() {
        let source = "activities { rule cargo { match \"^cargo\"; title Custom; }; }";
        let doc = crate::Document::parse(source).unwrap();
        let settings: Settings = serde_json::from_value(doc.value()["activities"].clone()).unwrap();
        assert_eq!(settings.rules[0].title, "Custom");
        assert_eq!(settings.rules[0].min_duration_ms, 0);
        assert_eq!(settings.rules.len(), 5);
        for invalid in [
            "activities { unknown 1; }",
            "activities { max-visible 3; }",
            "activities { show-badge 1; }",
            "activities { rule x { match \"[\"; title X; }; }",
            "activities { rule x { match \"(a)\"; title \"{2}\"; }; }",
            "activities { sources { typo #true; }; }",
        ] {
            assert!(
                crate::Document::parse(invalid)
                    .unwrap_err()
                    .to_string()
                    .contains("byte")
            );
        }
    }
}
