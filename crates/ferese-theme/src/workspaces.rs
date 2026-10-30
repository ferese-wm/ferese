use std::borrow::Cow;

use cosmic::iced::{Alignment, Color, Length};
use cosmic::widget::{Space, container, row};
use cosmic::{Element, theme};
use ferese_panel::WorkspaceStyle;

use crate::Palette;

#[derive(Clone)]
pub struct Workspace<'workspace> {
    pub name: Cow<'workspace, str>,
    pub index: u32,
    pub window_count: u32,
    pub active: bool,
    pub visible_elsewhere: bool,
}

#[must_use]
pub fn width(style: WorkspaceStyle) -> f32 {
    match style {
        WorkspaceStyle::Numbers | WorkspaceStyle::Dots => 24.,
        WorkspaceStyle::Tabs => 48.,
        WorkspaceStyle::WindowStacks => 32.,
        WorkspaceStyle::AppIcons => 48.,
    }
}

#[must_use]
pub fn tooltip_label(workspace: &Workspace<'_>) -> String {
    let name = if workspace.name.is_empty() || workspace.name == workspace.index.to_string() {
        String::new()
    } else {
        format!(" · {}", workspace.name)
    };
    format!(
        "Workspace {}{name} · {} {}{}",
        workspace.index,
        workspace.window_count,
        if workspace.window_count == 1 {
            "window"
        } else {
            "windows"
        },
        if workspace.active { " · Active" } else { "" },
    )
}

#[must_use]
pub fn indicator<'workspace, M: 'workspace>(
    style: WorkspaceStyle,
    workspace: Workspace<'workspace>,
    palette: Palette,
    font: cosmic::font::Font,
    height: f32,
    progress: f32,
    apps: Vec<Element<'workspace, M>>,
) -> Element<'workspace, M> {
    let progress = progress.clamp(0., 1.);
    let occupied = workspace.window_count > 0;
    let neutral = if occupied {
        palette.text
    } else {
        palette.muted
    };
    let foreground = crate::mix(neutral, palette.accent, progress);
    let content: Element<'workspace, M> = match style {
        WorkspaceStyle::Numbers | WorkspaceStyle::Tabs => {
            let label = if style == WorkspaceStyle::Tabs && !workspace.name.trim().is_empty() {
                workspace.name.into_owned()
            } else {
                workspace.index.to_string()
            };

            crate::text(label, font)
                .size(crate::panel::TEXT_SIZE)
                .line_height(cosmic::iced::widget::text::LineHeight::Absolute(
                    crate::panel::TEXT_LINE_HEIGHT.into(),
                ))
                .width(width(style) - 8.)
                .align_x(cosmic::iced::alignment::Horizontal::Center)
                .wrapping(cosmic::iced::widget::text::Wrapping::None)
                .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                    cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
                ))
                .class(theme::Text::Color(foreground))
                .into()
        }
        WorkspaceStyle::Dots => container(Space::new())
            .width(6. + 10. * progress)
            .height(6)
            .class(theme::Container::custom(move |_| {
                crate::controls::surface_appearance(
                    foreground.scale_alpha(if occupied {
                        0.65 + 0.35 * progress
                    } else {
                        0.18 + 0.82 * progress
                    }),
                    3.,
                )
            }))
            .into(),
        WorkspaceStyle::WindowStacks => {
            let back = match workspace.window_count {
                0 | 1 => "",
                2 => "<path d=\"M6 4h13a2 2 0 0 1 2 2v9\"/>",
                _ => "<path d=\"M6 4h13a2 2 0 0 1 2 2v9M9 1h13a2 2 0 0 1 2 2v9\"/>",
            };
            let svg = format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 26 22\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.4\" stroke-linejoin=\"round\">{back}<rect x=\"2\" y=\"7\" width=\"16\" height=\"12\" rx=\"2.5\"/>{}</svg>",
                if occupied {
                    "<path d=\"M2 11h16\"/>"
                } else {
                    ""
                }
            );

            cosmic::widget::icon::from_svg_bytes(crate::icons::tint_svg(
                svg.as_bytes(),
                foreground,
                foreground,
            ))
            .symbolic(false)
            .icon()
            .size(crate::panel::ICON_SIZE)
            .class(theme::Svg::custom(|_| cosmic::iced::widget::svg::Style {
                color: None,
            }))
            .into()
        }
        WorkspaceStyle::AppIcons => {
            let mut icons = row::with_capacity(2).spacing(3).align_y(Alignment::Center);

            if apps.is_empty() {
                icons = icons.push(crate::icons::tinted(crate::icons::OVERVIEW, 16, foreground));
            } else {
                let app_iter = apps.into_iter().take(2);
                for app in app_iter {
                    icons = icons.push(app);
                }
            }
            icons.into()
        }
    };

    let face = container(content).center_x(width(style)).center_y(height);
    let face: Element<'workspace, M> = if style == WorkspaceStyle::Dots {
        face.into()
    } else {
        let marker = container(Space::new())
            .width(10)
            .height(2)
            .class(crate::controls::surface(
                foreground.scale_alpha(progress),
                1.,
            ));
        cosmic::iced::widget::stack![
            face,
            container(marker)
                .width(Length::Fill)
                .height(Length::Fill)
                .align_x(cosmic::iced::alignment::Horizontal::Center)
                .align_y(cosmic::iced::alignment::Vertical::Bottom)
                .padding([1, 0]),
        ]
        .into()
    };

    container(face)
        .width(width(style))
        .height(height)
        .clip_to_border(true)
        .class(theme::Container::custom(move |_| {
            let fill = if style == WorkspaceStyle::Dots {
                Color::TRANSPARENT
            } else if progress > 0. {
                palette.accent.scale_alpha(0.16 * progress)
            } else if style == WorkspaceStyle::Tabs || occupied && style == WorkspaceStyle::Numbers
            {
                palette.text.scale_alpha(0.045)
            } else {
                Color::TRANSPARENT
            };

            crate::controls::surface_appearance(
                if workspace.visible_elsewhere {
                    palette.accent.scale_alpha(0.10)
                } else {
                    fill
                },
                palette.radius.min(8.),
            )
        }))
        .into()
}

#[must_use]
pub fn sample<'workspace, M: 'workspace>(
    style: WorkspaceStyle,
    palette: Palette,
    font: cosmic::font::Font,
    height: f32,
) -> Element<'workspace, M> {
    let mut samples = row::with_capacity(3).spacing(1).align_y(Alignment::Center);

    for (index, count, active) in [(1, 2, true), (2, 1, false), (3, 0, false)] {
        let apps = if style == WorkspaceStyle::AppIcons && count > 0 {
            let mut apps =
                vec![crate::icons::tinted(crate::icons::SETTINGS, 16, palette.text).into()];

            if count > 1 {
                apps.push(crate::icons::tinted(crate::icons::FERESE, 16, palette.accent).into());
            }

            apps
        } else {
            Vec::new()
        };

        samples = samples.push(indicator(
            style,
            Workspace {
                name: Cow::Owned(index.to_string()),
                index,
                window_count: count,
                active,
                visible_elsewhere: false,
            },
            palette,
            font,
            height,
            if active { 1. } else { 0. },
            apps,
        ));
    }
    samples.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic::iced::advanced::renderer::Headless;
    use cosmic::iced::advanced::widget::Tree;
    use cosmic::iced::advanced::{Layout, Renderer, layout, renderer};
    use cosmic::iced::{Font, Pixels, Rectangle, Size, mouse};

    #[test]
    fn indicators_keep_their_allocation_and_active_shape_in_every_style() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut renderer = runtime
            .block_on(<cosmic::Renderer as Headless>::new(
                Font::default(),
                Pixels(14.),
                Some("tiny-skia"),
            ))
            .unwrap();
        let palette = Palette::from_resolved(&ferese_config::theme::default_theme());
        let theme = palette.native_theme();
        for style in WorkspaceStyle::ALL {
            for height in [20., 28., 36.] {
                let bounds = Rectangle::with_size(Size::new(width(style), height));
                let mut active_alpha = Vec::new();
                let mut inactive_alpha = Vec::new();
                for (count, progress, name) in [
                    (0, 0., ""),
                    (1, 0., "1"),
                    (3, 0., "Design and development"),
                    (3, 0.5, "Design and development"),
                    (3, 1., "Design and development"),
                ] {
                    let apps = (0..count.min(2))
                        .map(|_| {
                            crate::icons::tinted(crate::icons::SETTINGS, 16, palette.text).into()
                        })
                        .collect();
                    let mut view: Element<'_, ()> = indicator(
                        style,
                        Workspace {
                            name: name.into(),
                            index: 1,
                            window_count: count,
                            active: progress == 1.,
                            visible_elsewhere: false,
                        },
                        palette,
                        Font::default(),
                        height,
                        progress,
                        apps,
                    );
                    let mut tree = Tree::new(&view);
                    let node = view.as_widget_mut().layout(
                        &mut tree,
                        &renderer,
                        &layout::Limits::new(Size::ZERO, bounds.size()),
                    );
                    assert_eq!(
                        node.bounds().size(),
                        bounds.size(),
                        "{style:?}, height={height}, count={count}, progress={progress}"
                    );
                    renderer.reset(bounds);
                    view.as_widget().draw(
                        &tree,
                        &mut renderer,
                        &theme,
                        &renderer::Style {
                            text_color: palette.text,
                            icon_color: palette.text,
                            scale_factor: 1.,
                        },
                        Layout::new(&node),
                        mouse::Cursor::Unavailable,
                        &bounds,
                    );
                    let pixels = Headless::screenshot(
                        &mut renderer,
                        Size::new(width(style) as u32, height as u32),
                        1.,
                        Color::TRANSPARENT,
                    );
                    let alpha: Vec<_> = pixels.chunks_exact(4).map(|p| p[3]).collect();
                    assert!(alpha.iter().any(|a| *a > 0), "{style:?} rendered empty");
                    if count == 3 && progress == 0. {
                        inactive_alpha = alpha;
                    } else if progress == 1. {
                        active_alpha = alpha;
                    }
                }
                assert_ne!(
                    active_alpha, inactive_alpha,
                    "{style:?} needs an active shape that survives removal of color"
                );
            }
        }
    }

    #[test]
    fn hover_details_include_names_and_pluralize_window_counts() {
        let mut workspace = Workspace {
            name: "Design".into(),
            index: 2,
            window_count: 1,
            active: true,
            visible_elsewhere: false,
        };
        assert_eq!(
            tooltip_label(&workspace),
            "Workspace 2 · Design · 1 window · Active"
        );
        workspace.name = "2".into();
        workspace.window_count = 0;
        workspace.active = false;
        assert_eq!(tooltip_label(&workspace), "Workspace 2 · 0 windows");
    }
}
