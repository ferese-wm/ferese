use super::{Action, HEIGHT, Model, Palette, fit, fixed_width, view_fitted};
use cosmic::Element;
use cosmic::iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, renderer, widget};
use cosmic::iced::{Event, Size, Vector};
use cosmic::iced::{Font, Length, Rectangle};
use cosmic::widget::container;
use std::rc::Rc;

type Callback<'a, M> = Rc<dyn Fn(Action, Rectangle<i32>) -> M + 'a>;

pub(super) fn view<'a, M: Clone + 'a>(
    model: Model,
    font: Font,
    palette: Palette,
    action: impl Fn(Action, Rectangle<i32>) -> M + 'a,
) -> Element<'a, M> {
    Element::new(Responsive {
        model,
        font,
        palette,
        action: Rc::new(action),
        content: cosmic::widget::Space::new().into(),
    })
}

struct Responsive<'a, M> {
    model: Model,
    font: Font,
    palette: Palette,
    action: Callback<'a, M>,
    content: Element<'a, M>,
}

#[derive(Default)]
struct State {
    controls: Vec<Option<Action>>,
}

impl<'content, M: Clone + 'content> Widget<M, cosmic::Theme, cosmic::Renderer>
    for Responsive<'content, M>
{
    fn tag(&self) -> widget::tree::Tag {
        widget::tree::Tag::of::<State>()
    }
    fn state(&self) -> widget::tree::State {
        widget::tree::State::new(State::default())
    }
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fixed(HEIGHT))
    }
    fn children(&self) -> Vec<widget::Tree> {
        vec![widget::Tree::new(&self.content)]
    }
    // The measured presentation is reconstructed in layout before diffing.
    fn diff(&mut self, _: &mut widget::Tree) {}
    fn layout(
        &mut self,
        tree: &mut widget::Tree,
        renderer: &cosmic::Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        let (model, menu) = fit(self.model.clone(), self.font, limits.max().width);
        let mut controls = model
            .workspaces
            .iter()
            .map(|workspace| workspace.id.map(Action::Workspace))
            .collect::<Vec<_>>();
        controls.extend(menu.then_some(Some(Action::Workspaces)));
        controls.extend(model.capture.iter().map(|(_, action)| Some(*action)));
        controls.extend(model.cpu.is_some().then_some(None));
        controls.extend(model.network.is_some().then_some(Some(Action::Network)));
        controls.extend(model.throughput.is_some().then_some(None));
        controls.extend(model.disk.is_some().then_some(None));
        controls.extend(model.volume.is_some().then_some(Some(Action::Audio)));
        controls.extend(model.battery.is_some().then_some(Some(Action::Battery)));
        controls.push(Some(Action::Settings));
        controls.extend((!model.clock.is_empty()).then_some(Some(Action::Calendar)));
        if !model.interactive {
            controls.clear();
        }
        let minimum = fixed_width(&model, self.font, menu);
        let action = self.action.clone();
        self.content = view_fitted(model, menu, self.font, self.palette, move |kind, bounds| {
            action(kind, bounds)
        });
        if minimum > limits.max().width {
            let content = std::mem::replace(&mut self.content, cosmic::widget::Space::new().into());
            self.content = cosmic::widget::scrollable(container(content).width(minimum))
                .direction(cosmic::iced::widget::scrollable::Direction::Horizontal(
                    cosmic::iced::widget::scrollable::Scrollbar::default()
                        .width(0)
                        .scroller_width(0),
                ))
                .width(Length::Fill)
                .height(HEIGHT)
                .into();
        }
        let state = tree.state.downcast_mut::<State>();
        if state.controls == controls {
            tree.children[0].diff(self.content.as_widget_mut());
        } else {
            // A service/geometry/workspace change must not transfer a held press
            // to whichever control now occupies the same child position.
            tree.children[0] = widget::Tree::new(&self.content);
            state.controls = controls;
        }
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
    }
    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut cosmic::Renderer,
        theme: &cosmic::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content.as_widget().draw(
            &tree.children[0],
            renderer,
            theme,
            style,
            layout,
            cursor,
            viewport,
        );
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
    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut widget::Tree,
        layout: Layout<'a>,
        renderer: &cosmic::Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<cosmic::iced::advanced::overlay::Element<'a, M, cosmic::Theme, cosmic::Renderer>>
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
