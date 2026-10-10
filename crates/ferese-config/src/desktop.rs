//! Shared desktop-widget configuration: validated before live publication.
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DesktopWidgets {
    pub clock: Clock,
    pub notes: Vec<StickyNote>,
}

impl DesktopWidgets {
    pub fn validate(&self) -> Result<(), String> {
        self.clock.validate()?;
        if self.notes.len() > 32 {
            return Err("At most 32 sticky notes are supported.".into());
        }
        let mut ids = std::collections::HashSet::new();
        for note in &self.notes {
            note.validate()?;
            if !ids.insert(&note.id) {
                return Err("Sticky note IDs must be unique.".into());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct StickyNote {
    pub id: String,
    pub enabled: bool,
    pub interactive: bool,
    pub title: String,
    pub text: String,
    pub outputs: Vec<String>,
    pub anchor: Anchor,
    pub margin_x: i32,
    pub margin_y: i32,
    pub width: u32,
    pub height: u32,
    pub font_family: Option<String>,
    pub text_size: f32,
    pub title_size: f32,
    pub color: Option<String>,
    pub background: Option<String>,
    pub opacity: f32,
    pub padding: f32,
    pub gap: f32,
    pub alignment: Alignment,
}

impl Default for StickyNote {
    fn default() -> Self {
        Self {
            id: "note".into(),
            enabled: true,
            interactive: true,
            title: "Note".into(),
            text: String::new(),
            outputs: Vec::new(),
            anchor: Anchor::TopRight,
            margin_x: 48,
            margin_y: 80,
            width: 320,
            height: 240,
            font_family: None,
            text_size: 16.,
            title_size: 18.,
            color: None,
            background: None,
            opacity: 0.9,
            padding: 20.,
            gap: 8.,
            alignment: Alignment::Left,
        }
    }
}

impl StickyNote {
    pub fn on_output(&self, name: Option<&str>) -> bool {
        self.enabled && (self.outputs.is_empty() || name.is_some_and(|name| self.outputs.iter().any(|s| s == name)))
    }
    /// Text/style changes repaint in place, never remap the desktop card.
    pub fn same_surface(&self, other: &Self) -> bool {
        self.id == other.id
            && self.enabled == other.enabled
            && self.interactive == other.interactive
            && self.outputs == other.outputs
            && self.anchor == other.anchor
            && self.margin_x == other.margin_x
            && self.margin_y == other.margin_y
            && self.width == other.width
            && self.height == other.height
    }

    pub fn validate(&self) -> Result<(), String> {
        let invalid = |field: &str| Err(format!("desktop_widgets.notes[{}].{field} is invalid", self.id));
        if self.id.is_empty()
            || self.id.len() > 64
            || !self
                .id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            return invalid("id");
        }
        if self.text.len() > 16384 || self.title.len() > 256 {
            return invalid("text/title");
        }
        if !(120..=1200).contains(&self.width) || !(80..=1200).contains(&self.height) {
            return invalid("width/height");
        }
        if !(0..=8192).contains(&self.margin_x) || !(0..=8192).contains(&self.margin_y) {
            return invalid("margin");
        }
        for (name, value, min, max) in [
            ("text_size", self.text_size, 8., 96.),
            ("title_size", self.title_size, 8., 96.),
            ("opacity", self.opacity, 0., 1.),
            ("padding", self.padding, 0., 64.),
            ("gap", self.gap, 0., 64.),
        ] {
            if !value.is_finite() || !(min..=max).contains(&value) {
                return invalid(name);
            }
        }
        for color in [&self.color, &self.background].into_iter().flatten() {
            if !color.is_empty()
                && color != "theme"
                && !color
                    .strip_prefix('#')
                    .is_some_and(|hex| matches!(hex.len(), 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                return invalid("color/background");
            }
        }
        if self.font_family.as_ref().is_some_and(|s| s.len() > 128)
            || self.outputs.len() > 32
            || self.outputs.iter().any(|s| s.is_empty() || s.len() > 128)
        {
            return invalid("font/outputs");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Anchor {
    #[default]
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    Center,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Alignment {
    Left,
    #[default]
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClockStyle {
    #[default]
    Pixel,
    Minimal,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Clock {
    pub style: ClockStyle,
    pub enabled: bool,
    /// Empty means every output; otherwise exact connector names.
    pub outputs: Vec<String>,
    pub anchor: Anchor,
    pub margin_x: i32,
    pub margin_y: i32,
    pub width: u32,
    pub height: u32,
    pub font_family: Option<String>,
    pub bold: bool,
    pub time_size: f32,
    pub date_size: f32,
    pub time_format: String,
    pub date_format: String,
    pub time_zone: Option<String>,
    pub show_date: bool,
    pub lowercase: bool,
    pub color: Option<String>,
    pub date_color: Option<String>,
    pub opacity: f32,
    pub alignment: Alignment,
    pub gap: f32,
    pub padding: f32,
    pub background: Option<String>,
}

impl Default for Clock {
    fn default() -> Self {
        Self {
            style: ClockStyle::Pixel,
            enabled: false,
            outputs: Vec::new(),
            anchor: Anchor::TopLeft,
            margin_x: 64,
            margin_y: 80,
            width: 440,
            height: 320,
            font_family: None,
            bold: false,
            time_size: 128.,
            date_size: 18.,
            time_format: "%-I:%M %p".into(),
            date_format: "%a, %b %-d".into(),
            time_zone: None,
            show_date: true,
            lowercase: true,
            color: None,
            date_color: None,
            opacity: 0.9,
            alignment: Alignment::Center,
            gap: 4.,
            padding: 12.,
            background: None,
        }
    }
}

impl Clock {
    pub fn on_output(&self, name: Option<&str>) -> bool {
        self.enabled
            && (self.outputs.is_empty() || name.is_some_and(|name| self.outputs.iter().any(|output| output == name)))
    }

    pub fn labels(&self, now: &jiff::Zoned) -> Result<(String, String), String> {
        let zoned = match &self.time_zone {
            Some(zone) if !zone.is_empty() => {
                now.with_time_zone(jiff::tz::TimeZone::get(zone).map_err(|e| e.to_string())?)
            }
            _ => now.clone(),
        };
        let time = jiff::fmt::strtime::format(&self.time_format, &zoned).map_err(|e| e.to_string())?;
        let date = jiff::fmt::strtime::format(&self.date_format, &zoned).map_err(|e| e.to_string())?;
        Ok(if self.lowercase {
            (time.to_lowercase(), date.to_lowercase())
        } else {
            (time, date)
        })
    }

    pub fn validate(&self) -> Result<(), String> {
        let invalid = |name: &str| Err(format!("desktop_widgets.clock.{name} is invalid"));
        if !(64..=1600).contains(&self.width) || !(32..=800).contains(&self.height) {
            return invalid("width/height");
        }
        if !(0..=8192).contains(&self.margin_x) || !(0..=8192).contains(&self.margin_y) {
            return invalid("margin_x/margin_y");
        }
        for (name, value, min, max) in [
            ("time_size", self.time_size, 8., 240.),
            ("date_size", self.date_size, 8., 96.),
            ("opacity", self.opacity, 0., 1.),
            ("gap", self.gap, 0., 64.),
            ("padding", self.padding, 0., 64.),
        ] {
            if !value.is_finite() || !(min..=max).contains(&value) {
                return invalid(name);
            }
        }
        for color in [&self.color, &self.date_color, &self.background].into_iter().flatten() {
            if color.is_empty() {
                continue;
            }
            if !color
                .strip_prefix('#')
                .is_some_and(|hex| matches!(hex.len(), 6 | 8) && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            {
                return invalid("color");
            }
        }
        if self.font_family.as_ref().is_some_and(|s| s.len() > 128)
            || self.time_format.len() > 128
            || self.date_format.len() > 128
            || self.outputs.len() > 32
            || self.outputs.iter().any(|s| s.is_empty() || s.len() > 128)
        {
            return invalid("font/format/outputs");
        }
        self.labels(&jiff::Zoned::now())
            .map(|_| ())
            .map_err(|e| format!("desktop_widgets.clock: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn widgets_reject_removed_radius_options() {
        assert!(crate::from_str::<DesktopWidgets>("clock { radius 12; }").is_err());
        assert!(crate::from_str::<DesktopWidgets>("note id=\"note\" { radius 12; }").is_err());
    }
    #[test]
    fn notes_reject_duplicate_ids_and_unbounded_content() {
        let note = StickyNote::default();
        note.validate().unwrap();
        assert!(
            DesktopWidgets {
                clock: Clock::default(),
                notes: vec![note.clone(), note.clone()]
            }
            .validate()
            .is_err()
        );
        assert!(
            StickyNote {
                text: "x".repeat(16385),
                ..note.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            StickyNote {
                opacity: f32::NAN,
                ..note.clone()
            }
            .validate()
            .is_err()
        );
        assert!(note.same_surface(&StickyNote {
            text: "edited".into(),
            ..note.clone()
        }));
    }

    #[test]
    fn defaults_are_safe_and_opt_in() {
        let clock = Clock::default();
        clock.validate().unwrap();
        assert!(!clock.on_output(Some("HDMI-A-1")));
    }

    #[test]
    fn output_filters_do_not_guess_monitor_names() {
        let clock = Clock {
            enabled: true,
            outputs: vec!["DP-2".into()],
            ..Clock::default()
        };
        assert!(clock.on_output(Some("DP-2")));
        assert!(!clock.on_output(Some("eDP-1")));
        assert!(!clock.on_output(None));
    }

    #[test]
    fn rejects_bad_sizes_colors_formats_and_zones() {
        for clock in [
            Clock {
                opacity: f32::NAN,
                ..Clock::default()
            },
            Clock {
                width: 8192,
                ..Clock::default()
            },
            Clock {
                color: Some("red".into()),
                ..Clock::default()
            },
            Clock {
                time_format: "%".into(),
                ..Clock::default()
            },
            Clock {
                time_zone: Some("not/a-zone".into()),
                ..Clock::default()
            },
        ] {
            assert!(clock.validate().is_err());
        }
    }

    #[test]
    fn formats_timezone_and_lowercase() {
        let now: jiff::Zoned = "2026-09-26T21:05:00+00:00[UTC]".parse().unwrap();
        let clock = Clock {
            time_zone: Some("Africa/Lagos".into()),
            date_format: "%-d %b".into(),
            ..Clock::default()
        };
        assert_eq!(clock.labels(&now).unwrap(), ("10:05 pm".into(), "26 sep".into()));
    }
}
