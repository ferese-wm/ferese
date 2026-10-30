use cosmic::iced::border::Shape as BorderShape;
use cosmic::iced::{Alignment, Border, Color, Length};
use cosmic::widget::{button, column, container, row, slider};
use cosmic::{Element, theme};

use crate::status::Action;
use crate::{Message, ShellTheme, color, motion};

pub(super) fn control_card<'a>(
    render: crate::rendering::Rendering,
    content: Element<'a, cosmic::Action<Message>>,
    foreground: Color,
    opacity: f32,
) -> cosmic::widget::Container<'a, cosmic::Action<Message>, cosmic::Theme, cosmic::Renderer> {
    container(content)
        .padding(10)
        .width(Length::Fill)
        .class(ferese_theme::controls::surface(
            foreground.scale_alpha(0.045 * opacity),
            render.radius(14.),
        ))
}

pub(super) fn menu_button<'a>(
    render: crate::rendering::Rendering,
    label: &'a str,
    message: Message,
    foreground: Color,
    opacity: f32,
) -> Element<'a, cosmic::Action<Message>> {
    motion::button(
        render,
        ferese_theme::menus::button(label, cosmic::Action::App(message), render.font),
        foreground,
        false,
        opacity,
    )
}

pub(super) fn shell_switch<'a>(
    render: crate::rendering::Rendering,
    label: &str,
    enabled: bool,
    action: Action,
    palette: ShellTheme,
    opacity: f32,
) -> Element<'a, cosmic::Action<Message>> {
    let toggle = ferese_theme::menus::switch(enabled, palette.palette(), opacity);
    motion::button(
        render,
        button::custom(toggle)
            .id(cosmic::widget::Id::new(format!("switch:{label}:{enabled}")))
            .padding(4)
            .name(format!("{label}: {}", if enabled { "on" } else { "off" }))
            .on_press(cosmic::Action::App(Message::Control(action))),
        color(palette.text_primary),
        false,
        opacity,
    )
}

pub(super) fn toggle_row<'a>(
    render: crate::rendering::Rendering,
    label: &'a str,
    on: bool,
    action: Action,
    palette: ShellTheme,
    opacity: f32,
) -> Element<'a, cosmic::Action<Message>> {
    ferese_theme::menus::row(
        label,
        shell_switch(render, label, on, action, palette, opacity),
        render.font,
    )
}

pub(super) fn level_control<'a>(
    render: crate::rendering::Rendering,
    label: &'static str,
    icon: Element<'a, cosmic::Action<Message>>,
    value: u8,
    range: std::ops::RangeInclusive<u8>,
    change: fn(u8) -> Action,
    style: super::MenuStyle,
) -> Element<'a, cosmic::Action<Message>> {
    let super::MenuStyle {
        primary,
        muted,
        opacity,
        ..
    } = style;
    let control = slider(range, value, move |value| {
        cosmic::Action::App(Message::Control(change(value)))
    })
    .width(Length::Fill)
    .height(20)
    .handle_width(8)
    .class(level_slider(primary));
    control_card(
        render,
        column![
            row![
                icon,
                render.text(label).size(12).width(Length::Fill),
                render
                    .text(format!("{value}%"))
                    .size(12)
                    .class(theme::Text::Color(muted)),
            ]
            .spacing(5)
            .align_y(Alignment::Center),
            control,
        ]
        .spacing(2)
        .into(),
        primary,
        opacity,
    )
    .padding([4, 10])
    .into()
}

pub(super) fn level_slider(foreground: Color) -> cosmic::theme::iced::Slider {
    let paint = std::rc::Rc::new(move |_: &cosmic::Theme| {
        use cosmic::iced::widget::slider::{Breakpoint, Handle, HandleShape, Rail, Style};
        Style {
            rail: Rail {
                backgrounds: (foreground.into(), foreground.scale_alpha(0.10).into()),
                width: 8.,
                border: Border {
                    shape: BorderShape::Circular,
                    radius: 4.into(),
                    ..Default::default()
                },
            },
            handle: Handle {
                corner_shape: BorderShape::Circular,
                shape: HandleShape::Circle { radius: 4. },
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
        active: paint.clone(),
        hovered: paint.clone(),
        dragging: paint,
    }
}
