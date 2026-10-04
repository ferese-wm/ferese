//! Shared menu layouts and paint; callers own actions and animation timing.
use cosmic::Element;
use cosmic::iced::border::Shape as BorderShape;
use cosmic::iced::{Alignment, Color, Length};
use cosmic::widget::{button, container, row as iced_row};

pub fn button<'a, M: Clone + 'a>(
    label: impl Into<std::borrow::Cow<'a, str>> + 'a,
    message: M,
    font: cosmic::font::Font,
) -> button::Button<'a, M> {
    button::custom(crate::text(label, font).size(13))
        .padding([6, 8])
        .on_press(message)
}

pub fn row<'a, M: 'a>(
    label: impl Into<std::borrow::Cow<'a, str>> + 'a,
    trailing: Element<'a, M>,
    font: cosmic::font::Font,
) -> Element<'a, M> {
    iced_row![crate::text(label, font).width(Length::Fill).size(14), trailing]
        .align_y(Alignment::Center)
        .into()
}

pub fn heading<'a, M: 'a>(
    badge: Element<'a, M>,
    title: impl Into<std::borrow::Cow<'a, str>> + 'a,
    font: cosmic::font::Font,
) -> cosmic::widget::Row<'a, M, cosmic::Theme, cosmic::Renderer> {
    iced_row![badge, crate::text(title, font).size(18).width(Length::Fill)]
        .spacing(10)
        .align_y(Alignment::Center)
}

pub fn badge<'a, M: 'a>(content: Element<'a, M>, accent: Color, opacity: f32, radius: f32) -> Element<'a, M> {
    container(content)
        .width(36)
        .height(36)
        .center_x(36)
        .center_y(36)
        .class(crate::controls::surface(
            accent.scale_alpha(0.12 * opacity),
            radius.min(20.),
        ))
        .into()
}

pub fn section<'a, M: 'a>(content: Element<'a, M>, foreground: Color, opacity: f32, radius: f32) -> Element<'a, M> {
    container(content)
        .padding(10)
        .width(Length::Fill)
        .class(crate::controls::surface(
            Color {
                a: 0.045 * opacity,
                ..foreground
            },
            radius.min(22.),
        ))
        .into()
}

pub fn section_label<'a>(
    label: impl Into<std::borrow::Cow<'a, str>> + 'a,
    font: cosmic::font::Font,
    muted: Color,
) -> cosmic::widget::Text<'a, cosmic::Theme, cosmic::Renderer> {
    crate::text(label, font)
        .size(13)
        .class(cosmic::theme::Text::Color(muted))
}

pub fn separator<'a, M: 'a>(foreground: Color, opacity: f32) -> Element<'a, M> {
    container(cosmic::widget::Space::new().width(Length::Fill).height(1))
        .class(crate::controls::surface(foreground.scale_alpha(0.12 * opacity), 0.))
        .into()
}

pub fn switch<'a, M: 'a>(enabled: bool, palette: crate::Palette, opacity: f32) -> Element<'a, M> {
    let (track, thumb) = crate::controls::switch_colors(palette, enabled, false);
    let track_radius = 10.;
    let padding = 2.;
    let knob = container(cosmic::widget::Space::new().width(16).height(16))
        .width(16)
        .height(16)
        .class(crate::controls::surface(
            Color { a: opacity, ..thumb },
            crate::inner_radius(track_radius, padding),
        ));
    container(knob)
        .width(36)
        .height(20)
        .padding(padding)
        .align_x(if enabled {
            cosmic::iced::alignment::Horizontal::Right
        } else {
            cosmic::iced::alignment::Horizontal::Left
        })
        .class(crate::controls::surface(track.scale_alpha(opacity), track_radius))
        .into()
}

pub fn slider(foreground: Color, opacity: f32) -> cosmic::theme::iced::Slider {
    let style = std::rc::Rc::new(move |_: &cosmic::Theme| {
        use cosmic::iced::widget::slider::{Breakpoint, Handle, HandleShape, Rail, Style};
        Style {
            rail: Rail {
                backgrounds: (
                    foreground.into(),
                    Color {
                        a: 0.16 * opacity,
                        ..foreground
                    }
                    .into(),
                ),
                width: 4.,
                border: cosmic::iced::Border {
                    shape: BorderShape::Continuous,
                    radius: 2.into(),
                    ..Default::default()
                },
            },
            handle: Handle {
                corner_shape: BorderShape::Circular,
                shape: HandleShape::Circle { radius: 6. },
                background: foreground.into(),
                border_width: 0.,
                border_color: Color::TRANSPARENT,
            },
            breakpoint: Breakpoint {
                color: Color::TRANSPARENT,
            },
        }
    });
    cosmic::theme::iced::Slider::Custom {
        active: style.clone(),
        hovered: style.clone(),
        dragging: style,
    }
}
