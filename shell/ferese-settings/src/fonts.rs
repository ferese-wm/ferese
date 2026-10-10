use std::process::Command;
use std::sync::OnceLock;

pub fn families() -> &'static [String] {
    static FAMILIES: OnceLock<Vec<String>> = OnceLock::new();

    FAMILIES.get_or_init(|| {
        let output = Command::new("fc-list")
            .args(["--format", "%{family}\\n"])
            .output()
            .ok()
            .filter(|output| output.status.success());
        let mut families: Vec<String> = output
            .as_ref()
            .map(|output| String::from_utf8_lossy(&output.stdout))
            .unwrap_or_default()
            .lines()
            .flat_map(|line| line.split(','))
            .map(str::trim)
            .filter(|family| !family.is_empty() && family.len() <= 128)
            .map(str::to_owned)
            .collect();

        families.extend(["sans-serif".into(), "serif".into(), "monospace".into()]);
        families.sort_unstable_by_key(|family| family.to_lowercase());
        families.dedup();
        families.insert(0, "Default font".into());
        families
    })
}

/// Missing theme fonts must fall back to a proportional interface font.
/// Iced does not apply fontconfig's named-family substitutions.
pub fn interface_font(family: &str) -> cosmic::iced::Font {
    match family {
        "sans-serif" => cosmic::iced::Font::DEFAULT,
        "serif" => cosmic::iced::Font {
            family: cosmic::iced::font::Family::Serif,
            ..cosmic::iced::Font::DEFAULT
        },
        "monospace" => cosmic::iced::Font::MONOSPACE,
        _ if families()
            .iter()
            .skip(1)
            .any(|installed| installed.eq_ignore_ascii_case(family)) =>
        {
            ferese_theme::font(Some(family))
        }
        _ => cosmic::iced::Font::DEFAULT,
    }
}
