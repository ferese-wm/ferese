use cosmic::Element;
use jiff::Zoned;

use super::controls::menu_button;
use crate::{Message, ShellTheme, color_with_opacity};

pub(super) fn view<'a>(
    render: crate::rendering::Rendering,
    offset: i32,
    theme: ShellTheme,
    opacity: f32,
) -> Element<'a, cosmic::Action<Message>> {
    let primary = color_with_opacity(theme.text_primary, opacity);
    ferese_theme::calendar::grid(
        Zoned::now().date(),
        offset,
        theme.palette(),
        render.font,
        opacity,
        [
            menu_button(render, "‹", Message::CalendarMonth(-1), primary, opacity),
            menu_button(render, "Today", Message::CalendarToday, primary, opacity),
            menu_button(render, "›", Message::CalendarMonth(1), primary, opacity),
        ],
    )
}

pub(super) fn hub<'a>(
    render: crate::rendering::Rendering,
    offset: i32,
    theme: ShellTheme,
    opacity: f32,
) -> Element<'a, cosmic::Action<Message>> {
    let primary = color_with_opacity(theme.text_primary, opacity);
    let navigation = |label: &'static str, message: Message| {
        crate::motion::button(
            render,
            cosmic::widget::button::custom(render.text(label).size(11).class(cosmic::theme::Text::Color(primary)))
                .name(label)
                .padding([2, 4])
                .height(22)
                .on_press(cosmic::Action::App(message)),
            primary,
            false,
            opacity,
        )
    };
    ferese_theme::calendar::compact_grid(
        Zoned::now().date(),
        offset,
        theme.palette(),
        render.font,
        opacity,
        [
            navigation("‹", Message::CalendarMonth(-1)),
            navigation("Today", Message::CalendarToday),
            navigation("›", Message::CalendarMonth(1)),
        ],
    )
}
