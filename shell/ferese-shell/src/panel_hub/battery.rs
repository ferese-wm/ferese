//! A fixed-width battery silhouette; the percentage is text, not regenerated SVG.
use super::*;
use cosmic::iced::{Background, Border};
use cosmic::widget::Space;

pub(super) const WIDTH: f64 = 34.;

impl FereseShell {
    pub(super) fn hub_battery(
        &self,
        id: window::Id,
        active: bool,
        opacity: f32,
    ) -> Option<Element<'_, cosmic::Action<Message>>> {
        let battery = self.status.battery.as_ref()?;
        let percent = battery.percent.min(100);
        let palette = self.config.theme.palette();
        let foreground = color_with_opacity(self.config.theme.text_primary, opacity);
        let outline = if self.hub_low_battery().is_some() {
            palette.error.scale_alpha(opacity)
        } else if matches!(battery.status.as_str(), "Charging" | "Pending charge") {
            palette.accent.scale_alpha(opacity)
        } else {
            foreground
        };
        let fill = container(Space::new().width(20. * f32::from(percent) / 100.).height(10)).class(
            theme::Container::custom(move |_| container::Style {
                background: Some(Background::Color(outline.scale_alpha(0.16))),
                border: Border {
                    shape: cosmic::iced::border::Shape::Continuous,
                    radius: 4.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
        );
        let number = self
            .render
            .text(percent.to_string())
            .size(9.5)
            .line_height(cosmic::iced::widget::text::LineHeight::Relative(1.))
            .class(theme::Text::Color(foreground));
        let body = container(cosmic::iced::widget::stack![
            container(fill).padding(2).width(24).height(14),
            container(number).center_x(24).center_y(14),
        ])
        .width(24)
        .height(14)
        .class(theme::Container::custom(move |_| container::Style {
            border: Border {
                color: outline,
                width: 1.2,
                shape: cosmic::iced::border::Shape::Continuous,
                radius: 5.5.into(),
                ..Default::default()
            },
            ..Default::default()
        }));
        let terminal =
            container(Space::new().width(1.5).height(4)).class(theme::Container::custom(move |_| container::Style {
                background: Some(Background::Color(outline)),
                border: Border {
                    radius: 1.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }));
        Some(motion::button(
            self.render,
            button::custom(row![body, terminal].spacing(1).align_y(Alignment::Center))
                .name(format!(
                    "Battery, {percent} percent, {}. Open battery controls",
                    battery.status
                ))
                .padding([7, 3])
                .width(WIDTH as f32)
                .height(28)
                .on_press(cosmic::Action::App(Message::HubOpen(id, status_ui::Menu::Battery))),
            foreground,
            active,
            opacity,
        ))
    }
}
