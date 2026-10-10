//! Measure item representations and resolve panel allocation for shell views and previews.
use cosmic::iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, renderer, widget};
use cosmic::iced::{Event, Length, Rectangle, Size, Vector};
use cosmic::{Element, Theme};
use ferese_config::panel::layout::{Measurement, Resolution, resolve_with_gap};
use ferese_config::panel::{ItemId, Panel, Representation};
use std::borrow::Cow;

/// Group outlines never paint a second fill over the panel background.
pub fn group_border(
    outline: cosmic::iced::Color,
    radius: impl Into<cosmic::iced::border::Radius>,
    enabled: bool,
) -> cosmic::widget::container::Style {
    let mut style = cosmic::widget::container::Style::default();
    style.border.radius = radius.into();
    if enabled {
        style.border.width = 1.;
        style.border.color = outline;
    }
    style
}

pub struct Sample<'a, M> {
    pub id: ItemId,
    pub representation: Representation,
    pub minimum: Option<f32>,
    pub view: Element<'a, M>,
}

pub struct Spacing {
    pub overflow_decoration: f32,
    pub zone_gap: f32,
}

impl From<f32> for Spacing {
    fn from(overflow_decoration: f32) -> Self {
        Self {
            overflow_decoration,
            zone_gap: 8.,
        }
    }
}

pub fn frame<'a, M: 'a>(
    panel: Cow<'a, Panel>,
    samples: Vec<Sample<'a, M>>,
    spacing: impl Into<Spacing>,
    initial: Resolution,
    build: impl Fn(&Resolution) -> Element<'a, M> + 'a,
    changed: impl Fn(Resolution) -> M + 'a,
) -> Element<'a, M> {
    let content = build(&initial);
    let spacing = spacing.into();
    Element::new(Adaptive {
        panel,
        samples,
        overflow_width: spacing.overflow_decoration,
        zone_gap: spacing.zone_gap,
        content,
        resolution: initial,
        build: Box::new(build),
        changed: Box::new(changed),
    })
}

#[derive(Default)]
struct State {
    published: Option<Resolution>,
}
type Build<'a, M> = Box<dyn Fn(&Resolution) -> Element<'a, M> + 'a>;
struct Adaptive<'a, M> {
    panel: Cow<'a, Panel>,
    samples: Vec<Sample<'a, M>>,
    overflow_width: f32,
    zone_gap: f32,
    content: Element<'a, M>,
    resolution: Resolution,
    build: Build<'a, M>,
    changed: Box<dyn Fn(Resolution) -> M + 'a>,
}

impl<M> Widget<M, Theme, cosmic::Renderer> for Adaptive<'_, M> {
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<State>()
    }
    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(State::default())
    }
    fn children(&self) -> Vec<widget::Tree> {
        std::iter::once(widget::Tree::new(&self.content))
            .chain(self.samples.iter().map(|sample| widget::Tree::new(&sample.view)))
            .collect()
    }
    fn diff(&mut self, tree: &mut widget::Tree) {
        tree.children.truncate(self.samples.len() + 1);
        if tree.children.is_empty() {
            tree.children.push(widget::Tree::new(&self.content));
        }
        tree.children[0].diff(self.content.as_widget_mut());
        for (index, sample) in self.samples.iter_mut().enumerate() {
            if tree.children.len() == index + 1 {
                tree.children.push(widget::Tree::new(&sample.view));
            }
            tree.children[index + 1].diff(sample.view.as_widget_mut());
        }
    }
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }
    fn layout(
        &mut self,
        tree: &mut widget::Tree,
        renderer: &cosmic::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let mut measurements: Vec<Measurement> = Vec::new();
        // Measure preferred content before imposing the panel's width. Otherwise
        // a clipped wide control can look as small as its compact alternative.
        let measurement_limits = layout::Limits::new(Size::ZERO, Size::new(f32::INFINITY, limits.max().height));
        for (index, sample) in self.samples.iter_mut().enumerate() {
            let node = sample
                .view
                .as_widget_mut()
                .layout(&mut tree.children[index + 1], renderer, &measurement_limits);
            let width = node.size().width;
            if let Some(measurement) = measurements.iter_mut().find(|measurement| measurement.id == sample.id) {
                measurement.alternatives.push((sample.representation, width));
            } else {
                measurements.push(Measurement {
                    id: sample.id.clone(),
                    alternatives: vec![(sample.representation, width)],
                    minimum: sample.minimum,
                });
            }
        }
        let trigger = measurements
            .iter()
            .find(|measurement| measurement.id.0 == "_overflow")
            .map_or(0.0, |measurement| measurement.alternatives[0].1);
        let trigger = trigger.min((limits.max().width - self.overflow_width).max(0.0));
        self.resolution = resolve_with_gap(
            &self.panel,
            &measurements,
            limits.max().width,
            (self.overflow_width + trigger).min(limits.max().width),
            self.zone_gap,
        );
        self.resolution.overflow_trigger_width = trigger;
        self.content = (self.build)(&self.resolution);
        tree.children[0].diff(self.content.as_widget_mut());
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }
    fn a11y_nodes(
        &self,
        layout: Layout<'_>,
        tree: &widget::Tree,
        cursor: mouse::Cursor,
    ) -> iced_accessibility::A11yTree {
        self.content.as_widget().a11y_nodes(layout, &tree.children[0], cursor)
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
        let state = tree.state.downcast_mut::<State>();
        if matches!(event, Event::Window(cosmic::iced::window::Event::RedrawRequested(_)))
            && state.published.as_ref() != Some(&self.resolution)
        {
            state.published = Some(self.resolution.clone());
            shell.publish((self.changed)(self.resolution.clone()));
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
        self.content
            .as_widget()
            .draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport);
    }
    fn mouse_interaction(
        &self,
        tree: &widget::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &cosmic::Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
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
    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut widget::Tree,
        layout: Layout<'a>,
        renderer: &cosmic::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<cosmic::iced::advanced::overlay::Element<'a, M, Theme, cosmic::Renderer>> {
        self.content
            .as_widget_mut()
            .overlay(&mut tree.children[0], layout, renderer, viewport, translation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic::iced::advanced::{clipboard, renderer::Headless};
    use cosmic::iced::{Font, Pixels, Point};
    use cosmic::widget::{button, text};
    use ferese_config::panel::OverflowPolicy;
    use ferese_config::panel::layout::Placement;

    #[derive(Clone, Debug)]
    enum Probe {
        Resolved(Resolution),
        Click,
        MeasurementClick,
    }

    #[test]
    fn group_borders_do_not_add_a_fill_or_change_content_colors() {
        use cosmic::iced::Color;
        let outline = Color::from_rgb(0.4, 0.5, 0.6);
        let enabled = group_border(outline, [0., 0., 12., 16.], true);
        let disabled = group_border(outline, [0., 0., 12., 16.], false);
        assert!(enabled.background.is_none() && disabled.background.is_none());
        assert_eq!(enabled.border.width, 1.);
        assert_eq!(enabled.border.color, outline);
        assert_eq!(disabled.border.width, 0.);
        assert_eq!(disabled.border.radius, enabled.border.radius);
        assert!(enabled.text_color.is_none() && enabled.icon_color.is_none());
        assert!(disabled.text_color.is_none() && disabled.icon_color.is_none());
    }

    #[test]
    fn drawing_an_outlined_group_does_not_raise_panel_background_alpha() {
        use cosmic::iced::advanced::{Renderer, renderer};
        use cosmic::iced::{Color, widget as widgets};
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
        let bounds = Rectangle::new(Point::ORIGIN, Size::new(40., 40.));
        for opacity in [0., 0.5, 1.] {
            for border in [false, true] {
                renderer.reset(bounds);
                let group = cosmic::widget::container(widgets::Space::new().width(40).height(40)).class(
                    cosmic::theme::Container::custom(move |_| group_border(Color::WHITE, 8., border)),
                );
                let mut view: Element<'_, ()> = cosmic::widget::container(group)
                    .class(cosmic::theme::Container::custom(move |_| {
                        crate::controls::surface_appearance(
                            Color {
                                a: opacity,
                                ..Color::WHITE
                            },
                            8.,
                        )
                    }))
                    .into();
                let mut tree = widget::Tree::new(&view);
                let node =
                    view.as_widget_mut()
                        .layout(&mut tree, &renderer, &layout::Limits::new(Size::ZERO, bounds.size()));
                view.as_widget().draw(
                    &tree,
                    &mut renderer,
                    &Theme::default(),
                    &renderer::Style {
                        text_color: Color::WHITE,
                        icon_color: Color::WHITE,
                        scale_factor: 1.,
                    },
                    Layout::new(&node),
                    mouse::Cursor::Unavailable,
                    &bounds,
                );
                let pixels = Headless::screenshot(&mut renderer, Size::new(40, 40), 1., Color::TRANSPARENT);
                let alpha = pixels[(20 * 40 + 20) * 4 + 3];
                assert!(
                    i16::from(alpha).abs_diff((opacity * 255.).round() as i16) <= 1,
                    "opacity={opacity}, border={border}, rendered alpha={alpha}"
                );
            }
        }
    }

    #[test]
    fn measured_widgets_adapt_publish_once_and_only_visible_controls_receive_input() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let renderer = runtime
            .block_on(<cosmic::Renderer as Headless>::new(
                Font::default(),
                Pixels(14.),
                Some("tiny-skia"),
            ))
            .unwrap();
        let mut panel = Panel::default();
        panel.start.groups.clear();
        panel.center.groups.clear();
        panel.end.groups.retain(|group| group.id.0 == "time");
        let id = ItemId("clock".into());
        let mut tree = None;
        for (width, always) in [(500., false), (75., false), (100., true), (500., false)] {
            panel.end.groups[0].items[0].overflow = if always {
                OverflowPolicy::Always
            } else {
                OverflowPolicy::Auto
            };
            let samples = [
                (Representation::Wide, "Wednesday, 7 October · 14:35"),
                (Representation::Compact, "14:35"),
                (Representation::Icon, "+"),
            ]
            .into_iter()
            .map(|(representation, label)| Sample {
                id: id.clone(),
                representation,
                minimum: None,
                view: button::custom(text(label).wrapping(cosmic::iced::widget::text::Wrapping::None))
                    .height(28)
                    .on_press(Probe::MeasurementClick)
                    .into(),
            })
            .chain(std::iter::once(Sample {
                id: ItemId("_overflow".into()),
                representation: Representation::Icon,
                minimum: None,
                view: button::custom(text("+"))
                    .height(28)
                    .on_press(Probe::MeasurementClick)
                    .into(),
            }))
            .collect();
            let id = id.clone();
            let mut view = frame(
                Cow::Borrowed(&panel),
                samples,
                0.,
                Resolution::default(),
                move |resolution| {
                    let width = match resolution.items.get(&id) {
                        Some(Placement::Visible { width, .. }) => *width,
                        _ => resolution.overflow_trigger_width,
                    };
                    button::custom(text("+"))
                        .width(width)
                        .height(28)
                        .on_press(Probe::Click)
                        .into()
                },
                Probe::Resolved,
            );
            let tree = tree.get_or_insert_with(|| widget::Tree::new(&view));
            tree.diff(&mut view);
            let viewport = Rectangle::new(Point::ORIGIN, Size::new(width, 28.));
            let limits = layout::Limits::new(Size::ZERO, viewport.size());
            let node = view.as_widget_mut().layout(tree, &renderer, &limits);
            let mut messages = Vec::new();
            for _ in 0..4 {
                view.as_widget_mut().update(
                    tree,
                    &Event::Window(cosmic::iced::window::Event::RedrawRequested(std::time::Instant::now())),
                    Layout::new(&node),
                    mouse::Cursor::Unavailable,
                    &renderer,
                    &mut clipboard::Null,
                    &mut Shell::new(&mut messages),
                    &viewport,
                );
            }
            assert_eq!(messages.len(), 1);
            let Probe::Resolved(resolution) = &messages[0] else {
                panic!("expected measured resolution")
            };
            assert!(resolution.fits(width));
            if always {
                assert_eq!(resolution.overflow, [ItemId("clock".into())]);
            } else if width == 500. {
                assert!(matches!(
                    resolution.items.get(&ItemId("clock".into())),
                    Some(Placement::Visible {
                        representation: Representation::Wide,
                        ..
                    })
                ));
            } else {
                assert!(matches!(
                    resolution.items.get(&ItemId("clock".into())),
                    Some(Placement::Visible {
                        representation: Representation::Compact | Representation::Icon,
                        ..
                    })
                ));
            }
            for event in [
                mouse::Event::ButtonPressed(mouse::Button::Left),
                mouse::Event::ButtonReleased(mouse::Button::Left),
            ] {
                view.as_widget_mut().update(
                    tree,
                    &Event::Mouse(event),
                    Layout::new(&node),
                    mouse::Cursor::Available(Point::new(5., 10.)),
                    &renderer,
                    &mut clipboard::Null,
                    &mut Shell::new(&mut messages),
                    &viewport,
                );
            }
            assert_eq!(
                messages
                    .iter()
                    .filter(|message| matches!(message, Probe::Click))
                    .count(),
                1
            );
            assert!(
                !messages
                    .iter()
                    .any(|message| matches!(message, Probe::MeasurementClick))
            );
        }
    }
}
