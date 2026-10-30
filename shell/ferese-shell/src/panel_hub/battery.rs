//! A fixed-width battery silhouette; the percentage is text, not regenerated SVG.
use super::{Element, FereseShell, Message, button, color_with_opacity, motion, status_ui, window};

pub(super) const WIDTH: f64 = 38.;

impl FereseShell {
    pub(super) fn hub_battery(
        &self,
        id: window::Id,
        active: bool,
        opacity: f32,
    ) -> Option<Element<'_, cosmic::Action<Message>>> {
        let battery = self.status.battery.as_ref()?;
        let percent = battery.percent.min(100);
        let foreground = color_with_opacity(self.config.theme.text_primary, opacity);
        Some(motion::button(
            self.render,
            button::custom(crate::bar_content(
                self.battery_indicator(self.config.theme, opacity, true, 14.),
                28.,
            ))
            .name(format!(
                "Battery, {percent} percent, {}. Open battery controls",
                battery.status
            ))
            .padding([0, 3])
            .width(WIDTH as f32)
            .height(28)
            .on_press(cosmic::Action::App(Message::HubOpen(
                id,
                status_ui::Menu::Battery,
            ))),
            foreground,
            active,
            opacity,
        ))
    }
}
