use cosmic::iced::advanced::graphics::geometry::{Cache, Path, Renderer as _, Stroke};
use cosmic::iced::advanced::{
    Clipboard, Layout, Renderer as _, Shell, Widget, layout, mouse, renderer, widget,
};
use cosmic::iced::{Color, Event, Length, Point, Rectangle, Size, Vector};
use cosmic::{Element, Theme};
use ferese_panel::scene::{GroupBounds, Parameters, Scene};
use ferese_panel::shape::{PixelGrid, ZonePosition};

pub struct Options {
    pub parameters: Parameters,
    pub bounds: Option<widget::Id>,
    pub groups: Vec<(widget::Id, ZonePosition)>,
    pub scale: f64,
    pub high_contrast: bool,
    pub background: Color,
    pub border: Color,
    pub divider: Color,
    pub native: bool,
}

pub fn frame<'content, M: 'content>(
    content: Element<'content, M>,
    options: Options,
    present: impl Fn(&Scene) + 'content,
    changed: Option<Changed<'content, M>>,
) -> Element<'content, M> {
    Element::new(Surface {
        content,
        options,
        present: Box::new(present),
        changed,
    })
}

type Present<'content> = Box<dyn Fn(&Scene) + 'content>;
type Changed<'content, M> = Box<dyn Fn(&Scene) -> M + 'content>;

struct Surface<'content, M> {
    content: Element<'content, M>,
    options: Options,
    present: Present<'content>,
    changed: Option<Changed<'content, M>>,
}

#[derive(Default)]
struct State {
    scene: Scene,
    cache: Cache<cosmic::Renderer>,
    colors: Option<(Color, Color, Color, bool, u64, bool)>,
    published: Option<Scene>,
}

struct Collect<'content> {
    targets: &'content [(widget::Id, ZonePosition)],
    origin: Point,
    groups: Vec<GroupBounds>,
    bounds_id: Option<&'content widget::Id>,
    bounds: Option<ferese_layout::Rect>,
}

impl widget::Operation for Collect<'_> {
    fn traverse(&mut self, children: &mut dyn FnMut(&mut dyn widget::Operation)) {
        children(self);
    }
    fn container(&mut self, id: Option<&widget::Id>, bounds: Rectangle) {
        if self.bounds_id.is_some() && id == self.bounds_id {
            self.bounds = Some(ferese_layout::Rect::new(
                (bounds.x - self.origin.x).into(),
                (bounds.y - self.origin.y).into(),
                bounds.width.into(),
                bounds.height.into(),
            ));
        }
        if let Some((_, zone)) = self.targets.iter().find(|(target, _)| Some(target) == id) {
            self.groups.push(GroupBounds {
                zone: *zone,
                bounds: ferese_layout::Rect::new(
                    (bounds.x - self.origin.x).into(),
                    (bounds.y - self.origin.y).into(),
                    bounds.width.into(),
                    bounds.height.into(),
                ),
            });
        }
    }
}

impl<M> Surface<'_, M> {
    fn measure(
        &mut self,
        tree: &mut widget::Tree,
        layout: Layout<'_>,
        renderer: &cosmic::Renderer,
    ) {
        let mut collector = Collect {
            targets: &self.options.groups,
            origin: layout.bounds().position(),
            groups: Vec::new(),
            bounds_id: self.options.bounds.as_ref(),
            bounds: None,
        };
        self.content.as_widget_mut().operate(
            &mut tree.children[0],
            layout,
            renderer,
            &mut collector,
        );
        let size = layout.bounds().size();
        let scene = PixelGrid::try_new(self.options.scale)
            .and_then(|grid| {
                ferese_panel::scene::derive(
                    self.options.parameters,
                    collector.bounds.unwrap_or_else(|| {
                        ferese_layout::Rect::new(0., 0., size.width.into(), size.height.into())
                    }),
                    &collector.groups,
                    grid,
                )
            })
            .unwrap_or_default();
        let state = tree.state.downcast_mut::<State>();
        let colors = (
            self.options.background,
            self.options.border,
            self.options.divider,
            self.options.native,
            self.options.scale.to_bits(),
            self.options.high_contrast,
        );
        if state.scene != scene || state.colors != Some(colors) {
            state.scene = scene;
            state.colors = Some(colors);
            state.cache.clear();
        }
    }
}

impl<M> Widget<M, Theme, cosmic::Renderer> for Surface<'_, M> {
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<State>()
    }
    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(State::default())
    }
    fn children(&self) -> Vec<widget::Tree> {
        vec![widget::Tree::new(&self.content)]
    }
    fn diff(&mut self, tree: &mut widget::Tree) {
        tree.diff_children(std::slice::from_mut(&mut self.content));
    }
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }
    fn layout(
        &mut self,
        tree: &mut widget::Tree,
        renderer: &cosmic::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let node = self
            .content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits);
        self.measure(tree, Layout::new(&node), renderer);
        node
    }
    fn a11y_nodes(
        &self,
        layout: Layout<'_>,
        tree: &widget::Tree,
        cursor: mouse::Cursor,
    ) -> iced_accessibility::A11yTree {
        self.content
            .as_widget()
            .a11y_nodes(layout, &tree.children[0], cursor)
    }
    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &cosmic::Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        self.content.as_widget_mut().update(
            &mut tree.children[0],
            event,
            layout,
            cursor,
            renderer,
            clipboard,
            shell,
            viewport,
        );
        if matches!(
            event,
            Event::Window(cosmic::iced::window::Event::RedrawRequested(_))
        ) && !shell.is_layout_invalid()
        {
            self.measure(tree, layout, renderer);
            (self.present)(&tree.state.downcast_ref::<State>().scene);
            let state = tree.state.downcast_mut::<State>();
            if state.published.as_ref() != Some(&state.scene) {
                if let Some(changed) = &self.changed {
                    shell.publish(changed(&state.scene));
                }
                state.published = Some(state.scene.clone());
            }
        }
    }
    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut cosmic::Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let state = tree.state.downcast_ref::<State>();
        let bounds = layout.bounds();
        if !self.options.native
            || self.options.border.a > 0.
            || (!state.scene.dividers.is_empty() && self.options.divider.a > 0.)
        {
            let geometry = state.cache.draw(renderer, bounds.size(), |frame| {
                let scale = if self.options.scale.is_finite() && self.options.scale > 0. {
                    self.options.scale
                } else {
                    1.
                };
                let hairline = 1. / scale as f32;
                let tolerance = (0.2 / scale).clamp(0.01, 1.);
                if let Ok(contours) = ferese_panel::outline::contours(&state.scene, tolerance) {
                    let path = Path::new(|builder| {
                        for contour in contours {
                            if let Some((first, rest)) = contour.split_first() {
                                builder.move_to(Point::new(first[0] as f32, first[1] as f32));
                                for point in rest {
                                    builder.line_to(Point::new(point[0] as f32, point[1] as f32));
                                }
                                builder.close();
                            }
                        }
                    });
                    if !self.options.native {
                        frame.fill(&path, self.options.background);
                    }
                    if self.options.border.a > 0. {
                        let width = if self.options.high_contrast {
                            hairline.max(1.)
                        } else {
                            hairline
                        };
                        if self.options.parameters.style == ferese_panel::PanelStyle::Flat {
                            let y = match self.options.parameters.edge {
                                ferese_panel::Edge::Top => bounds.height - width / 2.,
                                ferese_panel::Edge::Bottom => width / 2.,
                            };
                            frame.stroke(
                                &Path::line(Point::new(0., y), Point::new(bounds.width, y)),
                                Stroke::default()
                                    .with_color(self.options.border)
                                    .with_width(width),
                            );
                        } else if self.options.parameters.style == ferese_panel::PanelStyle::Notch
                            || (self.options.parameters.style == ferese_panel::PanelStyle::Morph
                                && self.options.parameters.attached)
                        {
                            if let Ok(contours) = ferese_panel::outline::notch_border(
                                &state.scene,
                                tolerance,
                                self.options.parameters.edge,
                            ) {
                                let border = Path::new(|builder| {
                                    for contour in contours {
                                        if let Some((first, rest)) = contour.split_first() {
                                            builder.move_to(Point::new(
                                                first[0] as f32,
                                                first[1] as f32,
                                            ));
                                            for point in rest {
                                                builder.line_to(Point::new(
                                                    point[0] as f32,
                                                    point[1] as f32,
                                                ));
                                            }
                                        }
                                    }
                                });
                                frame.stroke(
                                    &border,
                                    Stroke::default()
                                        .with_color(self.options.border)
                                        .with_width(width),
                                );
                            }
                        } else {
                            frame.stroke(
                                &path,
                                Stroke::default()
                                    .with_color(self.options.border)
                                    .with_width(width),
                            );
                        }
                    }
                }
                for [x1, y1, x2, y2] in &state.scene.dividers {
                    frame.stroke(
                        &Path::line(
                            Point::new(*x1 as f32, *y1 as f32),
                            Point::new(*x2 as f32, *y2 as f32),
                        ),
                        Stroke::default()
                            .with_color(self.options.divider)
                            .with_width(hairline),
                    );
                }
            });
            renderer.with_layer(*viewport, |renderer| {
                renderer.with_translation(Vector::new(bounds.x, bounds.y), |renderer| {
                    renderer.draw_geometry(geometry)
                })
            });
        }
        // Keep content after cached vector geometry in the renderer's layer order.
        // Parent-layer text can otherwise be batched underneath the background.
        renderer.with_layer(*viewport, |renderer| {
            self.content.as_widget().draw(
                &tree.children[0],
                renderer,
                theme,
                style,
                layout,
                cursor,
                viewport,
            );
        });
    }
    fn mouse_interaction(
        &self,
        tree: &widget::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &cosmic::Renderer,
    ) -> mouse::Interaction {
        self.content.as_widget().mouse_interaction(
            &tree.children[0],
            layout,
            cursor,
            viewport,
            renderer,
        )
    }
    fn operate(
        &mut self,
        tree: &mut widget::Tree,
        layout: Layout<'_>,
        renderer: &cosmic::Renderer,
        operation: &mut dyn widget::Operation,
    ) {
        self.content
            .as_widget_mut()
            .operate(&mut tree.children[0], layout, renderer, operation);
    }
    fn overlay<'content>(
        &'content mut self,
        tree: &'content mut widget::Tree,
        layout: Layout<'content>,
        renderer: &cosmic::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<cosmic::iced::advanced::overlay::Element<'content, M, Theme, cosmic::Renderer>>
    {
        self.content.as_widget_mut().overlay(
            &mut tree.children[0],
            layout,
            renderer,
            viewport,
            translation,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic::iced::advanced::renderer::Headless;
    use cosmic::iced::{Font, Pixels};
    use ferese_panel::shape::Style;

    #[test]
    fn shared_surface_publishes_layout_changes_and_leaves_native_fill_to_the_compositor() {
        use cosmic::iced::advanced::clipboard;
        use std::cell::RefCell;
        let mut renderer = cosmic::iced::futures::executor::block_on(
            <cosmic::Renderer as Headless>::new(Font::default(), Pixels(14.), Some("tiny-skia")),
        )
        .unwrap();
        let ids = [widget::Id::unique(), widget::Id::unique()];
        let mut tree = None;
        let viewport = Rectangle::with_size(Size::new(200., 32.));
        for (style, width, native) in [
            (Style::Floating, 40., false),
            (Style::Floating, 40., true),
            (Style::Floating, 60., true),
            (Style::Flat, 60., true),
        ] {
            let content = cosmic::widget::row::with_children([
                cosmic::widget::container(cosmic::widget::Space::new().width(width).height(32))
                    .id(ids[0].clone())
                    .into(),
                cosmic::widget::container(cosmic::widget::Space::new().width(40).height(32))
                    .id(ids[1].clone())
                    .into(),
            ])
            .spacing(20);
            let published = RefCell::new(Vec::new());
            let mut view = frame(
                cosmic::widget::container(content)
                    .width(200)
                    .height(32)
                    .into(),
                Options {
                    parameters: Parameters {
                        style,
                        height: 32.,
                        ..Default::default()
                    },
                    bounds: None,
                    groups: vec![
                        (ids[0].clone(), ZonePosition::Start),
                        (ids[1].clone(), ZonePosition::End),
                    ],
                    scale: 1.25,
                    high_contrast: false,
                    background: Color::WHITE,
                    border: Color::TRANSPARENT,
                    divider: Color::TRANSPARENT,
                    native,
                },
                |scene| published.borrow_mut().push(scene.clone()),
                Some(Box::new(Clone::clone)),
            );
            let tree = tree.get_or_insert_with(|| widget::Tree::new(&view));
            tree.diff(&mut view);
            let node = view.as_widget_mut().layout(
                tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, viewport.size()),
            );
            let mut messages = Vec::new();
            for _ in 0..3 {
                view.as_widget_mut().update(
                    tree,
                    &Event::Window(cosmic::iced::window::Event::RedrawRequested(
                        std::time::Instant::now(),
                    )),
                    Layout::new(&node),
                    mouse::Cursor::Unavailable,
                    &renderer,
                    &mut clipboard::Null,
                    &mut Shell::new(&mut messages),
                    &viewport,
                );
            }
            assert_eq!(published.borrow().len(), 3);
            assert_eq!(
                messages.len(),
                usize::from(!(style == Style::Floating && width == 40. && native))
            );
            let scene = published.borrow()[0].clone();
            assert_eq!(
                scene.input.len(),
                if style == Style::Floating { 2 } else { 1 }
            );
            if style == Style::Floating {
                assert!(
                    scene.distance([width as f64 + 10., 16.]) > 0.,
                    "the gap stays empty"
                );
            }
            renderer.reset(viewport);
            view.as_widget().draw(
                tree,
                &mut renderer,
                &Theme::default(),
                &renderer::Style {
                    text_color: Color::WHITE,
                    icon_color: Color::WHITE,
                    scale_factor: 1.25,
                },
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &viewport,
            );
            let pixels = renderer.screenshot(Size::new(250, 40), 1.25, Color::TRANSPARENT);
            assert_eq!(pixels.chunks_exact(4).any(|pixel| pixel[3] > 0), !native);
        }
    }

    #[test]
    fn border_coverage_tracks_physical_scale_and_high_contrast() {
        let mut renderer = cosmic::iced::futures::executor::block_on(
            <cosmic::Renderer as Headless>::new(Font::default(), Pixels(14.), Some("tiny-skia")),
        )
        .unwrap();
        for scale in [0.75, 1., 1.25, 1.5, 2.] {
            for high_contrast in [false, true] {
                let mut view: Element<'_, ()> = frame(
                    cosmic::widget::Space::new().width(64).height(32).into(),
                    Options {
                        parameters: Parameters {
                            style: Style::Flat,
                            edge: ferese_panel::Edge::Bottom,
                            ..Default::default()
                        },
                        bounds: None,
                        groups: Vec::new(),
                        scale,
                        high_contrast,
                        background: Color::TRANSPARENT,
                        border: Color::WHITE,
                        divider: Color::TRANSPARENT,
                        native: true,
                    },
                    |_| {},
                    None,
                );
                let mut tree = widget::Tree::new(&view);
                let bounds = Rectangle::with_size(Size::new(64., 32.));
                let node = view.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, bounds.size()),
                );
                renderer.reset(bounds);
                view.as_widget().draw(
                    &tree,
                    &mut renderer,
                    &Theme::default(),
                    &renderer::Style {
                        text_color: Color::WHITE,
                        icon_color: Color::WHITE,
                        scale_factor: scale,
                    },
                    Layout::new(&node),
                    mouse::Cursor::Unavailable,
                    &bounds,
                );
                let size = Size::new((64. * scale) as u32, (32. * scale) as u32);
                let pixels = renderer.screenshot(size, scale as f32, Color::TRANSPARENT);
                let coverage: f64 = (0..size.height)
                    .map(|y| {
                        f64::from(pixels[((y * size.width + size.width / 2) * 4 + 3) as usize])
                            / 255.
                    })
                    .sum();
                let expected = if high_contrast { scale.max(1.) } else { 1. };
                assert!(
                    (coverage - expected).abs() < 0.03,
                    "scale {scale}, high contrast {high_contrast}: {coverage} physical pixels, expected {expected}"
                );
            }
        }
    }

    #[test]
    fn the_surface_background_keeps_parent_layer_text_visible() {
        let mut renderer = cosmic::iced::futures::executor::block_on(
            <cosmic::Renderer as Headless>::new(Font::default(), Pixels(14.), Some("tiny-skia")),
        )
        .unwrap();
        for scale in [1., 1.25, 1.5] {
            let content = cosmic::widget::container(
                cosmic::widget::text("10:09").class(cosmic::theme::Text::Color(Color::WHITE)),
            )
            .center_x(150)
            .center_y(34);
            let mut view: Element<'_, ()> = frame(
                content.into(),
                Options {
                    parameters: Parameters {
                        style: Style::Morph,
                        height: 34.,
                        ..Default::default()
                    },
                    bounds: None,
                    groups: Vec::new(),
                    scale,
                    high_contrast: false,
                    background: Color::BLACK,
                    border: Color::TRANSPARENT,
                    divider: Color::TRANSPARENT,
                    native: false,
                },
                |_| {},
                None,
            );
            let mut tree = widget::Tree::new(&view);
            let viewport = Rectangle::with_size(Size::new(150., 34.));
            let node = view.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(Size::ZERO, viewport.size()),
            );
            renderer.reset(viewport);
            view.as_widget().draw(
                &tree,
                &mut renderer,
                &Theme::default(),
                &renderer::Style {
                    text_color: Color::WHITE,
                    icon_color: Color::WHITE,
                    scale_factor: scale,
                },
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &viewport,
            );
            let pixels = renderer.screenshot(
                Size::new((150. * scale) as u32, (34. * scale).ceil() as u32),
                scale as f32,
                Color::BLACK,
            );
            let ink = pixels
                .chunks_exact(4)
                .filter(|pixel| pixel[0] > 128)
                .count();
            assert!(
                ink > 25,
                "clock text was covered by the surface background at scale {scale}"
            );
        }
    }

    #[test]
    fn shared_surface_draws_core_outlines_at_fractional_scales() {
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
        let theme = Theme::default();
        for style in [
            Style::Floating,
            Style::Capsules,
            Style::Notch,
            Style::Flat,
            Style::Frame,
            Style::CornerWrap,
            Style::Fused,
        ] {
            for scale in [1., 1.25, 1.5] {
                let a = widget::Id::unique();
                let b = widget::Id::unique();
                let content = cosmic::widget::container(cosmic::widget::row![
                    cosmic::widget::Space::new().width(4),
                    cosmic::widget::container(cosmic::widget::Space::new().width(20).height(16))
                        .id(a.clone()),
                    cosmic::widget::Space::new().width(4),
                    cosmic::widget::container(cosmic::widget::Space::new().width(20).height(16))
                        .id(b.clone()),
                ])
                .width(96)
                .height(64)
                .into();
                let mut view: Element<'_, ()> = frame(
                    content,
                    Options {
                        parameters: Parameters {
                            style,
                            height: 16.,
                            gap: 4.,
                            frame_insets: [16., 4., 4., 4.],
                            frame_radius: 8.,
                            wrap_size: [40., 42.],
                            wrap_width: 8.,
                            fillet: 6.,
                            ..Default::default()
                        },
                        bounds: None,
                        groups: vec![(a, ZonePosition::Start), (b, ZonePosition::Start)],
                        scale,
                        high_contrast: false,
                        background: Color::WHITE,
                        border: Color::TRANSPARENT,
                        divider: Color::TRANSPARENT,
                        native: false,
                    },
                    |_| {},
                    None,
                );
                let mut tree = widget::Tree::new(&view);
                let bounds = Rectangle::new(Point::ORIGIN, Size::new(96., 64.));
                let node = view.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, bounds.size()),
                );
                renderer.reset(bounds);
                view.as_widget().draw(
                    &tree,
                    &mut renderer,
                    &theme,
                    &renderer::Style {
                        text_color: Color::WHITE,
                        icon_color: Color::WHITE,
                        scale_factor: scale,
                    },
                    Layout::new(&node),
                    mouse::Cursor::Unavailable,
                    &bounds,
                );
                let size = Size::new((96. * scale) as u32, (64. * scale) as u32);
                let pixels = Headless::screenshot(&mut renderer, size, scale as f32, Color::BLACK);
                let scene = &tree.state.downcast_ref::<State>().scene;
                assert!(!scene.paint.is_empty());
                for y in 0..size.height {
                    for x in 0..size.width {
                        let point = [(f64::from(x) + 0.5) / scale, (f64::from(y) + 0.5) / scale];
                        let distance = scene.distance(point);
                        if distance.abs() > 1. / scale {
                            let ink = pixels[((y * size.width + x) * 4) as usize] > 128;
                            assert_eq!(ink, distance < 0., "{style:?} scale {scale} at {point:?}");
                        }
                    }
                }
            }
        }
    }
}
