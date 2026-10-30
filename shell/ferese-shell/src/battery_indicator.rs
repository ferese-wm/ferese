use crate::{FereseShell, Message, ShellTheme, color_with_opacity};
use cosmic::iced::{Alignment, Background, Border};
use cosmic::widget::{Space, container, row};
use cosmic::{Element, theme};

const BODY_WIDTH: f32 = 28.;
pub(crate) const WIDTH: f32 = BODY_WIDTH + 1. + 1.5;

impl FereseShell {
    pub(crate) fn battery_indicator(
        &self,
        theme: ShellTheme,
        opacity: f32,
        show_percentage: bool,
        height: f32,
    ) -> Element<'static, cosmic::Action<Message>> {
        let scale = height / 14.;
        let body_width = BODY_WIDTH * scale;
        let battery = self.status.battery.as_ref();
        let percent = battery.map_or(0, |battery| battery.percent.min(100));
        let charging = battery.is_some_and(|battery| {
            matches!(battery.status.as_str(), "Charging" | "Pending charge")
        });
        let palette = theme.palette();
        let foreground = color_with_opacity(theme.text_primary, opacity);
        let outline = if battery.is_some_and(|battery| {
            battery.percent < self.config.status.low_battery_threshold
                && matches!(
                    battery.status.as_str(),
                    "Discharging" | "Pending discharge" | "Empty"
                )
        }) {
            palette.error.scale_alpha(opacity)
        } else if charging {
            palette.accent.scale_alpha(opacity)
        } else {
            foreground
        };
        let fill = container(
            Space::new()
                .width((BODY_WIDTH - 4.) * scale * f32::from(percent) / 100.)
                .height(10. * scale),
        )
        .class(theme::Container::custom(move |_| container::Style {
            background: Some(Background::Color(outline.scale_alpha(0.16))),
            border: Border {
                shape: cosmic::iced::border::Shape::Continuous,
                radius: (4. * scale).into(),
                ..Default::default()
            },
            ..Default::default()
        }));
        let label: Element<'static, cosmic::Action<Message>> = if show_percentage {
            self.render
                .text(battery.map_or_else(|| "–".into(), |_| percent.to_string()))
                .size(9.5 * scale)
                .line_height(cosmic::iced::widget::text::LineHeight::Relative(1.))
                .class(theme::Text::Color(foreground))
                .into()
        } else if charging {
            const BOLT: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 12"><path d="M4.5 0 0 7h3l-.5 5L8 5H5z" fill="currentColor"/></svg>"#;
            ferese_theme::icons::tinted(BOLT, (10. * scale).round() as u16, outline).into()
        } else {
            Space::new().into()
        };
        let body = container(cosmic::iced::widget::stack![
            container(fill)
                .padding(2. * scale)
                .width(body_width)
                .height(height),
            container(label).center_x(body_width).center_y(height),
        ])
        .width(body_width)
        .height(height)
        .class(theme::Container::custom(move |_| container::Style {
            border: Border {
                color: outline,
                width: 1.2 * scale,
                shape: cosmic::iced::border::Shape::Continuous,
                radius: (5.5 * scale).into(),
                ..Default::default()
            },
            ..Default::default()
        }));
        let terminal = container(Space::new().width(1.5 * scale).height(4. * scale)).class(
            theme::Container::custom(move |_| container::Style {
                background: Some(Background::Color(outline)),
                border: Border {
                    radius: scale.into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
        );
        row![body, terminal]
            .spacing(scale)
            .align_y(Alignment::Center)
            .into()
    }
}

pub(crate) fn icon(battery: Option<&crate::status::Battery>) -> &'static [u8] {
    use ferese_theme::icons;
    let Some(battery) = battery else {
        return icons::BATTERY_EMPTY;
    };
    if matches!(battery.status.as_str(), "Charging" | "Pending charge") {
        icons::BATTERY_CHARGING
    } else {
        match battery.percent {
            80.. => icons::BATTERY_FULL,
            50..=79 => icons::BATTERY_75,
            20..=49 => icons::BATTERY_50,
            1..=19 => icons::BATTERY_25,
            0 => icons::BATTERY_EMPTY,
        }
    }
}
