//! Compact bar components. The caller supplies live state, font and actions.
use cosmic::iced::{Alignment, Background, Color, Font, Length, Rectangle};
use cosmic::widget::{button, container, row};
use cosmic::{Element, theme};
use ferese_theme_model::{Appearance, ResolvedTheme};

mod responsive;

pub const HEIGHT: f32 = 24.;
pub const CELL_WIDTH: f32 = 28.;

#[derive(Clone, Copy)]
pub struct Palette {
    pub background: Color,
    pub border: Color,
    pub text: Color,
    pub bright: Color,
    pub muted: Color,
    pub divider: Color,
    pub accent: Color,
    pub on_accent: Color,
    pub urgent: Color,
    pub on_urgent: Color,
}

impl Palette {
    /// Use the presented theme, including bar-specific foreground and surface tokens.
    /// The text presentation is opaque even when other shell surfaces are translucent.
    ///
    /// # Panics
    /// Panics if a used theme color is invalid. Resolve and validate the theme first.
    #[must_use]
    pub fn from_resolved(theme: &ResolvedTheme) -> Self {
        let color =
            |value: &str| crate::parse_color(value).expect("validated resolved theme color");
        let bar = &theme.tokens.surface.bar;
        let colors = &theme.tokens.colors;
        let background = Color {
            a: 1.,
            ..color(&bar.background)
        };
        let text = readable_text(background, color(&bar.text_primary));
        let muted = readable_text(background, color(&bar.text_muted));
        let border = crate::composite(color(&colors.border), background);
        let accent = crate::composite(color(&colors.accent), background);
        // Urgency remains a semantic warning, independent of the selected accent.
        let urgent = match theme.appearance {
            Appearance::Dark => Color::from_rgb8(255, 107, 107),
            Appearance::Light => Color::from_rgb8(201, 42, 42),
        };
        Self {
            background,
            border,
            text,
            bright: text,
            muted,
            divider: border,
            accent,
            on_accent: readable_text(accent, color(&colors.on_accent)),
            urgent,
            on_urgent: readable_text(urgent, text),
        }
    }
}

// Preserve readable theme colors; otherwise move toward a readable shade only
// as far as needed, using the same gradual correction as the theme resolver.
fn readable_text(background: Color, preferred: Color) -> Color {
    let preferred = crate::composite(preferred, background);
    if crate::contrast(background, preferred) >= 4.5 {
        return preferred;
    }
    let rgba = |color: Color| [color.r, color.g, color.b, color.a].map(f64::from);
    let channels = ferese_theme_model::readable(rgba(preferred), rgba(background), 4.5)
        .map(|channel| (channel * 255.).round() as u8);
    Color::from_rgb8(channels[0], channels[1], channels[2])
}

#[derive(Clone, Debug)]
pub struct Workspace {
    pub index: u32,
    pub id: Option<u64>,
    pub active: bool,
    pub occupied: bool,
    pub urgent: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Workspace(u64),
    Workspaces,
    Network,
    Audio,
    Battery,
    Settings,
    Calendar,
    Capture,
    Recording,
}

#[derive(Clone)]
pub struct Model {
    pub workspaces: Vec<Workspace>,
    pub layout: String,
    pub title: String,
    pub cpu: Option<String>,
    pub network: Option<String>,
    pub throughput: Option<String>,
    pub disk: Option<String>,
    pub volume: Option<String>,
    pub battery: Option<String>,
    pub clock: String,
    /// Keep capture disclosure visible even though the normal bar items are replaced.
    pub capture: Vec<(String, Action)>,
    pub interactive: bool,
}

fn label<'a>(
    value: String,
    font: Font,
    color: Color,
) -> cosmic::widget::Text<'a, cosmic::Theme, cosmic::Renderer> {
    crate::text(value, font)
        .size(13)
        .class(theme::Text::Color(color))
        .line_height(cosmic::iced::widget::text::LineHeight::Absolute(18.into()))
        .wrapping(cosmic::iced::widget::text::Wrapping::None)
}

fn style(background: Color, foreground: Color) -> theme::Button {
    let base = move |focused: bool| button::Style {
        background: Some(Background::Color(background)),
        text_color: Some(foreground),
        border_radius: 0.into(),
        outline: None,
        overlay: crate::controls::focus_overlay(foreground, focused),
        ..Default::default()
    };
    theme::Button::Custom {
        active: Box::new(move |focus, _| base(focus)),
        hovered: Box::new(move |focus, _| button::Style {
            overlay: Some(Background::Color(Color {
                a: 0.10,
                ..foreground
            })),
            ..base(focus)
        }),
        pressed: Box::new(move |focus, _| button::Style {
            overlay: Some(Background::Color(Color {
                a: 0.18,
                ..foreground
            })),
            ..base(focus)
        }),
        disabled: Box::new(move |_| base(false)),
    }
}

/// Reusable by the live shell and Settings preview. No icon font or SVG is used.
pub fn view<'a, M: Clone + 'a>(
    model: Model,
    font: Font,
    palette: Palette,
    action: impl Fn(Action, Rectangle<i32>) -> M + Clone + 'a,
) -> Element<'a, M> {
    responsive::view(model, font, palette, action)
}

fn view_fitted<'a, M: Clone + 'a>(
    model: Model,
    workspace_menu: bool,
    font: Font,
    palette: Palette,
    action: impl Fn(Action, Rectangle<i32>) -> M + Clone + 'a,
) -> Element<'a, M> {
    let mut cells = row([]).spacing(0);
    for workspace in model.workspaces {
        let (background, foreground) = if workspace.urgent {
            (palette.urgent, palette.on_urgent)
        } else if workspace.active {
            (palette.accent, palette.on_accent)
        } else {
            (
                palette.background,
                if workspace.occupied {
                    palette.text
                } else {
                    palette.muted
                },
            )
        };
        let text = label(
            workspace.index.to_string(),
            Font {
                weight: if workspace.active {
                    cosmic::iced::font::Weight::Semibold
                } else {
                    font.weight
                },
                ..font
            },
            foreground,
        );
        let text: Element<'a, M> = container(text).center_x(CELL_WIDTH).center_y(HEIGHT).into();
        let content = if workspace.occupied {
            let marker = container(cosmic::widget::Space::new().width(4).height(4)).class(
                theme::Container::custom(move |_| container::Style {
                    background: Some(foreground.into()),
                    ..Default::default()
                }),
            );
            cosmic::iced::widget::stack![text, container(marker).padding(3)].into()
        } else {
            text
        };
        let mut cell = button::custom(content)
            .id(widget_id(format!(
                "workspace-{:?}-{}",
                workspace.id, workspace.index
            )))
            .name(format!(
                "Workspace {}{}{}{}",
                workspace.index,
                if workspace.active { ", current" } else { "" },
                if workspace.urgent { ", urgent" } else { "" },
                if workspace.id.is_none() {
                    ", unavailable"
                } else if workspace.occupied {
                    ", occupied"
                } else {
                    ", empty"
                }
            ))
            .padding(0)
            .width(CELL_WIDTH)
            .height(HEIGHT)
            .class(style(background, foreground));
        if model.interactive
            && let Some(id) = workspace.id
        {
            let action = action.clone();
            cell = cell.on_press_with_rectangle(move |_, _| {
                action(Action::Workspace(id), Rectangle::default())
            });
        }
        cells = cells.push(cell);
    }
    if workspace_menu {
        let mut control = button::custom(
            container(label("workspaces".into(), font, palette.text)).center_y(HEIGHT),
        )
        .id(widget_id("workspaces"))
        .name("All workspaces")
        .height(HEIGHT)
        .padding([0, 10])
        .class(style(palette.background, palette.text));
        if model.interactive {
            let action = action.clone();
            control = control.on_press_with_rectangle(move |offset, bounds| {
                action(Action::Workspaces, rectangle(offset, bounds))
            });
        }
        cells = cells.push(control);
    }
    let mut status = row([]).spacing(0).align_y(Alignment::Center);
    let mut segments = Vec::new();
    for (label, action) in model.capture {
        segments.push((label, Some(action), palette.urgent));
    }
    for (label, action) in [
        (model.cpu, None),
        (model.network, Some(Action::Network)),
        (model.throughput, None),
        (model.disk, None),
    ] {
        if let Some(label) = label {
            segments.push((label, action, palette.muted));
        }
    }
    if let Some(volume) = model.volume {
        segments.push((volume, Some(Action::Audio), palette.muted));
    }
    if let Some(battery) = model.battery {
        segments.push((battery, Some(Action::Battery), palette.muted));
    }
    segments.push(("settings".into(), Some(Action::Settings), palette.muted));
    if !model.clock.is_empty() {
        segments.push((model.clock, Some(Action::Calendar), palette.bright));
    }
    for (index, (text, kind, foreground)) in segments.into_iter().enumerate() {
        if index > 0 {
            status = status.push(label("|".into(), font, palette.divider));
        }
        let Some(kind) = kind else {
            status = status.push(
                container(label(text, font, foreground))
                    .center_y(HEIGHT)
                    .padding([0, 10]),
            );
            continue;
        };
        let mut control =
            button::custom(container(label(text.clone(), font, foreground)).center_y(HEIGHT))
                .id(widget_id(format!("{kind:?}")))
                .name(text)
                .padding([0, 10])
                .height(HEIGHT)
                .class(style(palette.background, foreground));
        if model.interactive {
            let action = action.clone();
            control = control.on_press_with_rectangle(move |offset, bounds| {
                action(kind, rectangle(offset, bounds))
            });
        }
        status = status.push(control);
    }
    let title = label(model.title, font, palette.bright)
        .width(Length::Fill)
        .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
            cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
        ));
    let content = row![
        cells,
        container(label(model.layout.clone(), font, palette.accent))
            .padding([0, if model.layout.is_empty() { 0 } else { 10 }]),
        title,
        status
    ]
    .spacing(0)
    .align_y(Alignment::Center)
    .width(Length::Fill)
    .height(HEIGHT);
    let background =
        container(content)
            .width(Length::Fill)
            .height(HEIGHT)
            .class(theme::Container::custom(move |_| container::Style {
                background: Some(palette.background.into()),
                ..Default::default()
            }));
    let rule = container(cosmic::widget::Space::new().height(1).width(Length::Fill)).class(
        theme::Container::custom(move |_| container::Style {
            background: Some(palette.border.into()),
            ..Default::default()
        }),
    );
    cosmic::iced::widget::stack![background, container(rule).align_bottom(HEIGHT)].into()
}

fn rectangle(offset: cosmic::iced::Vector, bounds: Rectangle) -> Rectangle<i32> {
    Rectangle {
        x: (bounds.x - offset.x).round() as i32,
        y: (bounds.y - offset.y).round() as i32,
        width: bounds.width.round() as i32,
        height: bounds.height.round() as i32,
    }
}

fn text_width(content: &str, font: Font) -> f32 {
    use cosmic::iced::advanced::text::{self, Paragraph};
    <cosmic::Renderer as text::Renderer>::Paragraph::with_text(text::Text {
        content,
        font,
        bounds: cosmic::iced::Size::INFINITE,
        size: 13.into(),
        line_height: text::LineHeight::Absolute(18.into()),
        align_x: Default::default(),
        align_y: cosmic::iced::alignment::Vertical::Top,
        shaping: Default::default(),
        wrapping: text::Wrapping::None,
        ellipsize: Default::default(),
    })
    .min_width()
    .ceil()
}

fn fixed_width(model: &Model, font: Font, workspace_menu: bool) -> f32 {
    let labels = model
        .capture
        .iter()
        .map(|(label, _)| label.as_str())
        .chain(model.cpu.as_deref())
        .chain(model.network.as_deref())
        .chain(model.throughput.as_deref())
        .chain(model.disk.as_deref())
        .chain(model.volume.as_deref())
        .chain(model.battery.as_deref())
        .chain(std::iter::once("settings"))
        .chain((!model.clock.is_empty()).then_some(model.clock.as_str()))
        .collect::<Vec<_>>();
    model.workspaces.len() as f32 * CELL_WIDTH
        + if workspace_menu {
            text_width("workspaces", font) + 20.
        } else {
            0.
        }
        + if model.layout.is_empty() {
            0.
        } else {
            text_width(&model.layout, font) + 20.
        }
        + labels
            .iter()
            .map(|label| text_width(label, font) + 20.)
            .sum::<f32>()
        + labels.len().saturating_sub(1) as f32 * text_width("|", font)
}

/// Shorten the title first (the row's flexible child), then optional status text.
/// On narrow outputs all workspaces remain available through a text menu.
fn fit(mut model: Model, font: Font, available: f32) -> (Model, bool) {
    if fixed_width(&model, font, false) <= available {
        return (model, false);
    }
    model.clock = model.clock.split_whitespace().last().unwrap_or("").into();
    for step in 0..9 {
        if fixed_width(&model, font, false) <= available {
            return (model, false);
        }
        match step {
            0 => model.battery = None,
            1 => model.volume = None,
            2 => model.disk = None,
            3 => model.cpu = None,
            4 => model.throughput = None,
            5 => model.network = None,
            6 => model.layout.clear(),
            7 => model.clock.clear(),
            _ => model.workspaces.retain(|workspace| workspace.active),
        }
    }
    if fixed_width(&model, font, true) > available {
        model.workspaces.clear();
    }
    (model, true)
}

fn widget_id(value: impl AsRef<str>) -> cosmic::iced::advanced::widget::Id {
    cosmic::iced::advanced::widget::Id::new(format!("text-bar:{}", value.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_uses_bar_specific_tokens_and_current_accent() {
        let mut theme = ferese_config::theme::default_theme();
        theme.tokens.surface.bar.background = "#102030".into();
        theme.tokens.surface.bar.text_primary = "#F4EEE2".into();
        theme.tokens.surface.bar.text_muted = "#BAC8D0".into();
        theme.tokens.colors.surface_base = "#080808".into();
        theme.tokens.colors.text_primary = "#FFFFFF".into();
        theme.tokens.colors.accent = "#E0A060".into();
        theme.tokens.colors.on_accent = "#102030".into();
        theme.tokens.colors.border = "#223344".into();
        let palette = Palette::from_resolved(&theme);
        assert_eq!(palette.background, Color::from_rgb8(16, 32, 48));
        assert_eq!(palette.text, Color::from_rgb8(244, 238, 226));
        assert_eq!(palette.bright, palette.text);
        assert_eq!(palette.muted, Color::from_rgb8(186, 200, 208));
        assert_eq!(palette.accent, Color::from_rgb8(224, 160, 96));
        assert_eq!(palette.on_accent, Color::from_rgb8(16, 32, 48));
        assert_eq!(palette.border, Color::from_rgb8(34, 51, 68));
        assert_eq!(palette.divider, palette.border);
    }

    #[test]
    fn insufficient_text_contrast_keeps_the_theme_tint_instead_of_using_white() {
        let mut theme = ferese_config::theme::default_theme();
        theme.tokens.surface.bar.background = "#101820".into();
        theme.tokens.surface.bar.text_primary = "#D8D8DC".into();
        theme.tokens.surface.bar.text_muted = "#404858".into();
        theme.tokens.colors.accent = "#202830".into();
        theme.tokens.colors.on_accent = "#404858".into();
        let palette = Palette::from_resolved(&theme);
        assert_eq!(palette.text, Color::from_rgb8(216, 216, 220));
        for (background, foreground) in [
            (palette.background, palette.muted),
            (palette.accent, palette.on_accent),
        ] {
            assert_ne!(
                foreground,
                Color::WHITE,
                "contrast correction must not jump to white"
            );
            assert!(
                foreground.b > foreground.r,
                "preserve the theme's cool tint"
            );
            assert!(crate::contrast(background, foreground) >= 4.5);
        }
        assert!(crate::luminance(palette.muted) < crate::luminance(palette.text));
    }

    #[test]
    fn light_theme_fallback_stays_tinted_and_readable_colors_are_unchanged() {
        let mut theme = ferese_config::theme::default_theme();
        theme.appearance = Appearance::Light;
        theme.tokens.surface.bar.background = "#F0F0F4".into();
        theme.tokens.surface.bar.text_primary = "#202838".into();
        theme.tokens.surface.bar.text_muted = "#C0C8D8".into();
        let palette = Palette::from_resolved(&theme);
        assert_eq!(palette.text, Color::from_rgb8(32, 40, 56));
        assert_ne!(palette.muted, Color::BLACK);
        assert!(palette.muted.b > palette.muted.r);
        assert!(crate::contrast(palette.background, palette.muted) >= 4.5);
    }

    #[test]
    fn translucent_tokens_keep_the_bar_opaque_and_cell_labels_readable() {
        let mut theme = ferese_config::theme::default_theme();
        theme.tokens.material.style = "translucent".into();
        theme.tokens.material.opacity = 0.;
        theme.tokens.surface.bar.background = "#10203000".into();
        theme.tokens.colors.accent = "#F8A04080".into();
        theme.tokens.colors.on_accent = "#F8A04000".into();
        let palette = Palette::from_resolved(&theme);
        assert_eq!(palette.background, Color::from_rgb8(16, 32, 48));
        assert_eq!(palette.accent.into_rgba8(), [132, 96, 56, 255]);
        for color in [
            palette.background,
            palette.border,
            palette.text,
            palette.bright,
            palette.muted,
            palette.divider,
            palette.accent,
            palette.on_accent,
            palette.urgent,
            palette.on_urgent,
        ] {
            assert_eq!(color.a, 1., "text-bar colors must remain opaque");
        }
        for (background, foreground) in [
            (palette.accent, palette.on_accent),
            (palette.urgent, palette.on_urgent),
        ] {
            assert!(
                crate::contrast(background, foreground) >= 4.5,
                "filled cell labels must remain readable"
            );
        }
    }
}
