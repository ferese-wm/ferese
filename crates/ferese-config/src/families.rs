use crate::theme::Appearance;
pub use ferese_theme_model::families::{Family, Palette};

pub const BUILTINS: [(&str, &str, &str, &str); 6] = [
    (
        "ferese-blue",
        "Ferese Blue",
        "ferese-blue-light",
        "ferese-blue",
    ),
    (
        "catppuccin",
        "Catppuccin",
        "catppuccin-latte",
        "catppuccin-mocha",
    ),
    ("gruvbox", "Gruvbox", "gruvbox-light", "gruvbox"),
    ("rose-pine", "Rosé Pine", "rose-pine-dawn", "rose-pine-moon"),
    (
        "tokyo-night",
        "Tokyo Night",
        "tokyo-night-day",
        "tokyo-night",
    ),
    (
        "everforest",
        "Everforest",
        "everforest-light",
        "everforest-dark",
    ),
];

#[must_use]
pub fn family_id(preset: &str) -> &str {
    BUILTINS
        .iter()
        .find(|row| row.2 == preset || row.3 == preset)
        .map_or(preset, |row| row.0)
}

#[must_use]
pub fn variant(id: &str, appearance: Appearance) -> Option<&'static str> {
    if let Some(row) = BUILTINS.iter().find(|row| row.0 == id) {
        return Some(if appearance == Appearance::Light {
            row.2
        } else {
            row.3
        });
    }
    match (id, appearance) {
        ("dracula", Appearance::Dark) => Some("dracula"),
        ("monochrome", Appearance::Dark) => Some("monochrome"),
        ("monokai", Appearance::Dark) => Some("monokai"),
        ("ayu-light", Appearance::Light) => Some("ayu-light"),
        _ => None,
    }
}

#[must_use]
pub fn builtins() -> Vec<Family> {
    BUILTINS
        .iter()
        .map(|(id, name, light, dark)| Family {
            id: (*id).into(),
            name: (*name).into(),
            light: Some(Palette::from(
                &crate::theme::preset(light, Appearance::Light)
                    .expect("built-in theme families must reference registered presets"),
            )),
            dark: Some(Palette::from(
                &crate::theme::preset(dark, Appearance::Dark)
                    .expect("built-in theme families must reference registered presets"),
            )),
        })
        .collect()
}
