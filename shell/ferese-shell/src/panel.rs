pub(crate) use ferese_config::panel::*;

pub(crate) fn from_status(status: &crate::config::StatusConfig) -> Panel {
    Panel::from_defaults(&Defaults {
        bar_layout: status.bar_layout,
        bar_island_padding: status.bar_island_padding,
        window_title: status.window_title,
        battery_percentage: status.battery_percentage,
    })
}
