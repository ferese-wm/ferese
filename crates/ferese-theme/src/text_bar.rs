//! Compact bar components. The caller supplies live state, font and actions.
use cosmic::iced::{Alignment, Background, Color, Font, Length, Rectangle};
use cosmic::widget::{button, container, row};
use cosmic::{Element, theme};
use ferese_theme_model::Appearance;

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
    /// Both variants follow the shared resolver's effective appearance.
    #[must_use]
    pub fn new(appearance: Appearance) -> Self {
        let rgb = |hex: u32| Color::from_rgb8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8);
        let [
            background,
            border,
            text,
            bright,
            muted,
            divider,
            accent,
            on_accent,
            urgent,
            on_urgent,
        ] = match appearance {
            Appearance::Dark => [
                0x14181f, 0x222a36, 0xc9d1d9, 0xe6edf3, 0x7d8898, 0x3a4352, 0x5b8cff, 0x0b1020,
                0xff6b6b, 0x1a0b0b,
            ],
            Appearance::Light => [
                0xeef1f6, 0xd3dae5, 0x1c2430, 0x1c2430, 0x5f6b7a, 0xaab4c3, 0x2f5fd0, 0xffffff,
                0xc92a2a, 0xffffff,
            ],
        }
        .map(rgb);
        Self {
            background,
            border,
            text,
            bright,
            muted,
            divider,
            accent,
            on_accent,
            urgent,
            on_urgent,
        }
    }
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
