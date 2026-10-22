use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

pub fn font(family: Option<&str>) -> cosmic::font::Font {
    let Some(family) = family else {
        return cosmic::font::default();
    };
    static FONTS: OnceLock<Mutex<HashMap<String, cosmic::font::Font>>> = OnceLock::new();
    let mut fonts = FONTS
        .get_or_init(Default::default)
        .lock()
        .expect("shared font cache must not be used after a mutator panics");
    if let Some(font) = fonts.get(family) {
        return *font;
    }
    if fonts.len() >= 64 {
        return cosmic::font::default();
    }
    let name = Box::leak(family.to_owned().into_boxed_str());
    let font = cosmic::font::Font::with_name(name);
    fonts.insert(family.to_owned(), font);
    font
}

/// COSMIC's text helper overrides Settings::default_font; set the Ferese font explicitly.
pub fn text<'a>(
    content: impl Into<std::borrow::Cow<'a, str>> + 'a,
    font: cosmic::font::Font,
) -> cosmic::widget::Text<'a, cosmic::Theme, cosmic::Renderer> {
    cosmic::widget::text(content).font(font)
}
