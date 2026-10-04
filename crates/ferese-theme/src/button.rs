use cosmic::iced::border::Shape as BorderShape;
use cosmic::iced::{Background, Color};
use cosmic::theme;
use cosmic::widget::button;

use crate::{composite, foreground};

#[derive(Clone, Copy)]
pub(crate) enum State {
    Active,
    Hovered,
    Pressed,
    Disabled,
}

pub(crate) fn accent_button_style(theme: &cosmic::Theme, state: State, focused: bool) -> button::Style {
    let native = theme.cosmic();
    let component = &native.accent_button;
    let fill: Color = match state {
        State::Active => component.base.into(),
        State::Hovered => component.hover.into(),
        State::Pressed => component.pressed.into(),
        State::Disabled => Color {
            a: 0.5,
            ..component.base.into()
        },
    };
    let background = composite(fill, native.primary(false).base.into());
    let foreground = foreground(background, component.on.into());
    button::Style {
        shape: Some(BorderShape::Continuous),
        outline: None,
        background: Some(Background::Color(background)),
        text_color: Some(foreground),
        icon_color: Some(foreground),
        border_radius: native.corner_radii.radius_xl.into(),
        outline_width: if focused { 1. } else { 0. },
        outline_color: foreground,
        ..Default::default()
    }
}

/// Filled accent buttons and selected rows share this per-state contrast policy.
pub fn accent_button() -> theme::Button {
    theme::Button::Custom {
        active: Box::new(|focused, theme| accent_button_style(theme, State::Active, focused)),
        hovered: Box::new(|focused, theme| accent_button_style(theme, State::Hovered, focused)),
        pressed: Box::new(|focused, theme| accent_button_style(theme, State::Pressed, focused)),
        disabled: Box::new(|theme| accent_button_style(theme, State::Disabled, false)),
    }
}
