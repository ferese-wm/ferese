use crate::{FereseShell, Message, ShellTheme, color_with_opacity};
use cosmic::iced::{Alignment, Background, Border};
use cosmic::widget::{Space, container, row};
use cosmic::{Element, theme};

const BODY_WIDTH: f32 = 28.;

impl FereseShell {
    pub(crate) fn battery_indicator(
        &self,
        theme: ShellTheme,
        opacity: f32,
        show_percentage: bool,
    ) -> Element<'static, cosmic::Action<Message>> {
        let battery = self.status.battery.as_ref();
        let percent = battery.map_or(0, |battery| battery.percent.min(100));
        let palette = theme.palette();
        let foreground = color_with_opacity(theme.text_primary, opacity);
        let outline = if battery.is_some_and(|battery| {
            battery.percent < self.config.status.low_battery_threshold
                && matches!(battery.status.as_str(), "Discharging" | "Pending discharge" | "Empty")
        }) {
            palette.error.scale_alpha(opacity)
        } else if battery.is_some_and(|battery| matches!(battery.status.as_str(), "Charging" | "Pending charge")) {
            palette.accent.scale_alpha(opacity)
        } else {
            foreground
        };
        let fill = container(
            Space::new()
                .width((BODY_WIDTH - 4.) * f32::from(percent) / 100.)
                .height(10),
        )
        .class(theme::Container::custom(move |_| container::Style {
            background: Some(Background::Color(outline.scale_alpha(0.16))),
            border: Border {
                shape: cosmic::iced::border::Shape::Continuous,
                radius: 4.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }));
        let number = self
            .render
            .text(if show_percentage {
                battery.map_or_else(|| "–".into(), |_| percent.to_string())
            } else {
                String::new()
            })
            .size(9.5)
            .line_height(cosmic::iced::widget::text::LineHeight::Relative(1.))
            .class(theme::Text::Color(foreground));
        let body = container(cosmic::iced::widget::stack![
            container(fill).padding(2).width(BODY_WIDTH).height(14),
            container(number).center_x(BODY_WIDTH).center_y(14),
        ])
        .width(BODY_WIDTH)
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
        row![body, terminal].spacing(1).align_y(Alignment::Center).into()
    }
}
