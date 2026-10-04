use cosmic::iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, renderer, widget};
use cosmic::iced::{Event, Length, Rectangle, Size, Vector};
use cosmic::{Element, Theme};
use ferese_config::BarLayout;

pub(crate) fn island_id() -> widget::Id {
    static ID: std::sync::OnceLock<widget::Id> = std::sync::OnceLock::new();
    ID.get_or_init(widget::Id::unique).clone()
}

pub(super) fn frame<'a, M: 'a>(
    content: Element<'a, M>,
    mode: BarLayout,
    radius: f32,
    present: impl Fn(&[[f32; 5]]) + 'a,
    changed: impl Fn(Vec<[f32; 5]>) -> M + 'a,
) -> Element<'a, M> {
    Element::new(Presentation {
        content,
        mode,
        radius,
        present: Box::new(present),
        changed: Box::new(changed),
    })
}

#[derive(Default)]
struct State {
    mode: Option<BarLayout>,
    current: Vec<[f32; 5]>,
    scratch: Vec<[f32; 5]>,
    scroll_starts: Vec<usize>,
}

type Present<'a> = Box<dyn Fn(&[[f32; 5]]) + 'a>;

struct Presentation<'a, M> {
    content: Element<'a, M>,
    mode: BarLayout,
    radius: f32,
    present: Present<'a>,
    changed: Box<dyn Fn(Vec<[f32; 5]>) -> M + 'a>,
}

impl<M> Widget<M, Theme, cosmic::Renderer> for Presentation<'_, M> {
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
        if !matches!(event, Event::Window(cosmic::iced::window::Event::RedrawRequested(_))) {
            return;
        }

        let state = tree.state.downcast_mut::<State>();
        let mut collector = Collect {
            regions: std::mem::take(&mut state.scratch),
            scroll_starts: std::mem::take(&mut state.scroll_starts),
            radius: self.radius,
        };
        collector.regions.clear();
        collector.scroll_starts.clear();
        if self.mode == BarLayout::Islands {
            self.content
                .as_widget_mut()
                .operate(&mut tree.children[0], layout, renderer, &mut collector);
        } else {
            let bounds = layout.bounds();
            collector
                .regions
                .push([bounds.x, bounds.y, bounds.width, bounds.height, self.radius]);
        }

        collector.clip_from(0, layout.bounds(), Vector::ZERO);
        collector.regions.retain(|region| region[2] > 0.0 && region[3] > 0.0);
        (self.present)(&collector.regions);
        if state.mode != Some(self.mode) || state.current != collector.regions {
            state.mode = Some(self.mode);
            state.current.clone_from(&collector.regions);
            shell.publish((self.changed)(collector.regions.clone()));
        }

        state.scratch = collector.regions;
        state.scroll_starts = collector.scroll_starts;
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

struct Collect {
    regions: Vec<[f32; 5]>,
    scroll_starts: Vec<usize>,
    radius: f32,
}

impl Collect {
    fn clip_from(&mut self, start: usize, viewport: Rectangle, translation: Vector) {
        for region in &mut self.regions[start..] {
            let bounds = Rectangle {
                x: region[0] - translation.x,
                y: region[1] - translation.y,
                width: region[2],
                height: region[3],
            };
            let bounds = bounds.intersection(&viewport).unwrap_or_default();
            region[..4].copy_from_slice(&[bounds.x, bounds.y, bounds.width, bounds.height]);
        }
    }
}

impl widget::Operation for Collect {
    fn traverse(&mut self, children: &mut dyn FnMut(&mut dyn widget::Operation)) {
        children(self);
    }

    fn container(&mut self, id: Option<&widget::Id>, bounds: Rectangle) {
        if id == Some(&island_id()) {
            self.regions
                .push([bounds.x, bounds.y, bounds.width, bounds.height, self.radius]);
        }
    }

    fn pre_operation(&mut self, _id: Option<&widget::Id>) {
        // Iced visits scrollable content before its scrollable callback. Record
        // the range so only that subtree receives its offset and viewport clip.
        self.scroll_starts.push(self.regions.len());
    }

    fn scrollable(
        &mut self,
        _id: Option<&widget::Id>,
        bounds: Rectangle,
        _content: Rectangle,
        translation: Vector,
        _state: &mut dyn widget::operation::Scrollable,
    ) {
        if let Some(start) = self.scroll_starts.pop() {
            self.clip_from(start, bounds, translation);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic::iced::advanced::{clipboard, renderer::Headless};
    use cosmic::iced::{Font, Pixels, widget as widgets};
    use cosmic::widget::{container, row};

    #[test]
    fn measured_islands_and_live_layout_switches_publish_only_changed_bounds() {
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
        let viewport = Rectangle::new((0., 0.).into(), (600., 28.).into());
        let mut tree = None;
        let theme = crate::ShellTheme {
            bar_height: 28.,
            panel_padding: 12.,
            bar_radius: 14.,
            ..Default::default()
        };

        for (mode, padding) in [
            (BarLayout::Islands, 12.),
            (BarLayout::Islands, 4.),
            (BarLayout::Islands, 0.),
            (BarLayout::Continuous, 32.),
            (BarLayout::Islands, 12.),
        ] {
            let section = |width| {
                let inner = container(widgets::Space::new().width(width).height(24))
                    .class(cosmic::theme::Container::custom(move |_| {
                        crate::bar::bar_group_style(theme)
                    }))
                    .into();
                crate::bar::island(inner, theme, padding, mode == BarLayout::Islands, true)
            };
            let islands = row![section(100), section(200),].spacing(60);
            let content = container(islands)
                .padding([0, 12])
                .width(600)
                .height(28)
                .align_y(cosmic::iced::alignment::Vertical::Center)
                .into();
            let mut view = frame(
                content,
                mode,
                14.,
                |_| {},
                |regions| {
                    cosmic::Action::App(crate::Message::BarRegionsChanged(
                        cosmic::iced::window::Id::unique(),
                        regions,
                    ))
                },
            );
            let tree = tree.get_or_insert_with(|| widget::Tree::new(&view));
            tree.diff(&mut view);
            let node = view
                .as_widget_mut()
                .layout(tree, &renderer, &layout::Limits::new(Size::ZERO, viewport.size()));
            let mut messages = Vec::new();

            for _ in 0..5 {
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

            assert_eq!(messages.len(), 1, "unchanged frames must not rebuild input regions");
            let expected = if mode == BarLayout::Islands {
                vec![
                    [12., 0., 100. + 2. * padding, 28., 14.],
                    [172. + 2. * padding, 0., 200. + 2. * padding, 28., 14.],
                ]
            } else {
                vec![[0., 0., 600., 28., 14.]]
            };
            let cosmic::Action::App(crate::Message::BarRegionsChanged(_, regions)) = &messages[0] else {
                panic!("expected measured bar regions");
            };
            assert_eq!(*regions, expected);
        }
    }

    #[test]
    fn scrolling_clips_only_its_own_islands_and_preserves_fractional_positions() {
        let mut collector = Collect {
            regions: vec![[400., 2., 80., 24., 14.], [10.25, 2., 180., 24., 14.]],
            scroll_starts: Vec::new(),
            radius: 14.,
        };
        collector.clip_from(
            1,
            Rectangle::new((20., 0.).into(), (100., 28.).into()),
            Vector::new(40.5, 0.),
        );
        assert_eq!(
            collector.regions,
            vec![[400., 2., 80., 24., 14.], [20., 2., 100., 24., 14.]]
        );
        collector.clip_from(0, Rectangle::new((0., 0.).into(), (200., 28.).into()), Vector::ZERO);
        collector.regions.retain(|region| region[2] > 0. && region[3] > 0.);
        assert_eq!(collector.regions, vec![[20., 2., 100., 24., 14.]]);
    }
}
