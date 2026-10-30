use cosmic::font::Font;

#[derive(Clone, Copy)]
pub(crate) struct Rendering {
    pub font: Font,
    pub motion: crate::motion::Settings,
    radius: f32,
}

impl Rendering {
    pub fn new(config: &crate::config::ShellConfig) -> Self {
        Self {
            font: crate::configured_font(config.font_family.as_deref()),
            motion: config.animations,
            radius: config.theme.material_radius,
        }
    }

    pub fn radius(self, limit: f32) -> f32 {
        self.radius.min(limit)
    }

    pub fn text<'a>(
        self,
        content: impl Into<std::borrow::Cow<'a, str>> + 'a,
    ) -> cosmic::widget::Text<'a, cosmic::Theme, cosmic::Renderer> {
        ferese_theme::text(content, self.font)
    }

    pub fn panel_text<'a>(
        self,
        content: impl Into<std::borrow::Cow<'a, str>> + 'a,
    ) -> cosmic::widget::Text<'a, cosmic::Theme, cosmic::Renderer> {
        self.text(content)
            .size(ferese_theme::panel::TEXT_SIZE)
            .line_height(cosmic::iced::widget::text::LineHeight::Absolute(
                ferese_theme::panel::TEXT_LINE_HEIGHT.into(),
            ))
    }
}
