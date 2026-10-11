use cosmic::Element;
use jiff::Zoned;

use super::controls::menu_button;
use crate::{Message, ShellTheme, color_with_opacity, shell_font};

pub(super) fn view<'a>(offset: i32, theme: ShellTheme, opacity: f32) -> Element<'a, cosmic::Action<Message>> {
    let primary = color_with_opacity(theme.text_primary, opacity);
    ferese_theme::calendar::grid(
        Zoned::now().date(),
        offset,
        theme.palette(),
        shell_font(),
        opacity,
        [
            menu_button("‹", Message::CalendarMonth(-1), primary, opacity),
            menu_button("Today", Message::CalendarToday, primary, opacity),
            menu_button("›", Message::CalendarMonth(1), primary, opacity),
        ],
    )
}

pub(super) fn hub<'a>(offset: i32, theme: ShellTheme, opacity: f32) -> Element<'a, cosmic::Action<Message>> {
    let primary = color_with_opacity(theme.text_primary, opacity);
    let navigation = |label: &'static str, message: Message| {
        crate::motion::button(
            cosmic::widget::button::custom(crate::text(label).size(11).class(cosmic::theme::Text::Color(primary)))
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
        shell_font(),
        opacity,
        [
            navigation("‹", Message::CalendarMonth(-1)),
            navigation("Today", Message::CalendarToday),
            navigation("›", Message::CalendarMonth(1)),
        ],
    )
}
