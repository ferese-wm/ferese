use cosmic::iced::advanced::widget::{Id, Operation, Tree, tree};
use cosmic::iced::advanced::{Clipboard, Layout, Shell, Widget, layout, mouse, renderer};
use cosmic::iced::border::Shape as BorderShape;
use cosmic::iced::{Event, Length, Rectangle, Size, keyboard};
use cosmic::{Element, Renderer, Theme};
use ferese_theme_model::Appearance;
use ferese_theme_model::families::{Family, Palette};
use iced_accessibility::{A11yTree, accesskit};

pub struct TileOptions {
    pub variant: Option<Appearance>,
    pub active: Option<Appearance>,
    pub selected: bool,
    pub palette: crate::Palette,
    pub font: cosmic::font::Font,
    pub id: Id,
}

pub fn tile<M: Clone + 'static>(
    family: &Family,
    options: TileOptions,
    on_select: M,
    navigate: impl Fn(&keyboard::key::Named) -> Option<M> + 'static,
) -> Element<'static, M> {
    let TileOptions {
        variant,
        active,
        selected,
        palette,
        font,
        id,
    } = options;
    use cosmic::widget::{button, container, row};
    let label_color = family
        .palette(variant.unwrap_or(Appearance::Light))
        .or(family.dark.as_ref())
        .map(|palette| crate::parse_color(&palette.text).unwrap())
        .unwrap_or(palette.text);
    let mut caption = row([]).spacing(6).align_y(cosmic::iced::Alignment::Center).push(
        crate::text(family.name.clone(), font)
            .size(12)
            .class(cosmic::theme::Text::Color(label_color)),
    );
    if family.light.is_none() || family.dark.is_none() {
        caption = caption.push(
            crate::text(
                if family.light.is_none() {
                    "Dark only"
                } else {
                    "Light only"
                },
                font,
            )
            .size(10)
            .class(cosmic::theme::Text::Color(label_color)),
        );
    }
    let family_preview = family.clone();
    let caption: Element<'static, M> = container(caption)
        .padding(8)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_y(cosmic::iced::alignment::Vertical::Bottom)
        .into();
    let image = cosmic::widget::responsive(move |size| {
        let height = size.width * 126. / 200.;
        // Match the button's logical radius when scaling the cached SVG artwork.
        let radius = 10. * 200. / size.width.max(1.);
        preview_with_radius(&family_preview, variant, active, selected, radius)
            .height(Length::Fixed(height))
            .into()
    })
    .height(Length::Shrink);
    let content = cosmic::iced::widget::stack([image.into(), caption]);
    let tile = button::custom(content)
        .id(id.clone())
        .name(format!("{}, {}", family.name, family.availability()))
        .width(Length::Fill)
        .padding(0)
        .class(tile_style(palette, selected))
        .on_press(on_select);
    radio(tile, id, selected, navigate)
}

fn tile_style(palette: crate::Palette, selected: bool) -> cosmic::theme::Button {
    let style = move |focused: bool, hovered: bool| cosmic::widget::button::Style {
        shape: Some(BorderShape::Continuous),
        outline: None,
        background: None,
        border_width: if selected || focused {
            2.
        } else if hovered {
            1.
        } else {
            0.
        },
        border_color: palette.accent,
        border_radius: 10.into(),
        outline_width: 0.,
        outline_color: palette.text,
        text_color: Some(palette.text),
        icon_color: Some(palette.text),
        ..Default::default()
    };
    cosmic::theme::Button::Custom {
        active: Box::new(move |focused, _| style(focused, false)),
        hovered: Box::new(move |focused, _| style(focused, true)),
        pressed: Box::new(move |focused, _| style(focused, true)),
        disabled: Box::new(move |_| style(false, false)),
    }
}

pub fn preview(
    family: &Family,
    variant: Option<Appearance>,
    active: Option<Appearance>,
    selected: bool,
) -> cosmic::widget::icon::Icon {
    preview_with_radius(family, variant, active, selected, 10.)
}

fn preview_with_radius(
    family: &Family,
    variant: Option<Appearance>,
    active: Option<Appearance>,
    selected: bool,
    radius: f32,
) -> cosmic::widget::icon::Icon {
    use cosmic::widget::icon;
    let key = format!("{}/{variant:?}/{active:?}/{selected}/{}", family.id, radius.to_bits());
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, (Family, icon::Handle)>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let mut cache = cache.lock().unwrap();
    if cache.get(&key).is_none_or(|(cached, _)| cached != family) {
        let handle = icon::from_svg_bytes(svg_with_radius(family, variant, active, selected, radius).into_bytes())
            .symbolic(false);
        cache.insert(key.clone(), (family.clone(), handle));
    }
    let handle = cache[&key].1.clone();
    // Bound cached previews when users repeatedly edit imported palettes.
    if cache.len() > 128 {
        cache.clear();
    }
    handle
        .icon()
        .width(Length::Fill)
        .height(Length::Fixed(112.))
        .content_fit(cosmic::iced::ContentFit::Fill)
}

fn desktop(p: &Palette) -> String {
    format!(
        r#"<rect width="200" height="126" fill="{}"/>
<rect x="8" y="8" width="184" height="14" rx="4" fill="{}" stroke="{}" stroke-opacity=".18"/>
<path d="M15 15h10m145 0h15" stroke="{}" stroke-width="2" stroke-linecap="round"/>
<rect x="24" y="35" width="152" height="77" rx="7" fill="{}"/>
<path d="M24 51h152" stroke="{}" stroke-opacity=".22"/>
<path d="M35 44h34M36 63h105m-105 10h90m-90 10h115" stroke="{}" stroke-width="2" stroke-linecap="round"/>
<rect x="76" y="92" width="48" height="10" rx="3" fill="{}"/>"#,
        p.background, p.bar, p.muted, p.muted, p.surface, p.muted, p.text, p.accent
    )
}

pub fn svg(family: &Family, variant: Option<Appearance>, active: Option<Appearance>, selected: bool) -> String {
    svg_with_radius(family, variant, active, selected, 10.)
}

fn svg_with_radius(
    family: &Family,
    variant: Option<Appearance>,
    active: Option<Appearance>,
    selected: bool,
    radius: f32,
) -> String {
    let mut result = String::from(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="126" viewBox="0 0 200 126"><defs>
<clipPath id="light"><rect width="100" height="126"/></clipPath><clipPath id="dark"><rect x="100" width="100" height="126"/></clipPath>
<pattern id="missing" width="8" height="8" patternUnits="userSpaceOnUse"><rect width="8" height="8" fill="#D9DDE2"/><path d="M-2 2l4-4M0 8l8-8M6 10l4-4" stroke="#C4C9D1"/></pattern>"##,
    );
    let outline =
        cosmic::iced::border::Outline::new([0., 0., 200., 126.], [f64::from(radius); 4], BorderShape::Continuous)
            .expect("finite preview dimensions");
    // Rasterized once per cached preview; no shaped rendering layer per tile.
    let points = outline.polygon(0.02).expect("positive contour tolerance");
    result.push_str("<clipPath id=\"preview\"><polygon points=\"");
    for [x, y] in points {
        use std::fmt::Write as _;
        write!(result, "{x:.4},{y:.4} ").unwrap();
    }
    result.push_str("\"/></clipPath></defs><g clip-path=\"url(#preview)\">");
    for appearance in [Appearance::Light, Appearance::Dark] {
        if variant.is_some_and(|variant| variant != appearance) {
            continue;
        }
        let clip = match (variant, appearance) {
            (None, Appearance::Light) => "light",
            (None, Appearance::Dark) => "dark",
            _ => "",
        };
        result.push_str(&format!(
            "<g{}>",
            if clip.is_empty() {
                String::new()
            } else {
                format!(" clip-path=\"url(#{clip})\"")
            }
        ));
        if let Some(palette) = family.palette(appearance) {
            result.push_str(&desktop(palette));
            if active == Some(appearance) {
                let (x, width) = if variant.is_some() {
                    (8, 184)
                } else if appearance == Appearance::Light {
                    (8, 84)
                } else {
                    (108, 84)
                };
                result.push_str(&format!(
                    "<rect x=\"{x}\" y=\"120\" width=\"{width}\" height=\"3\" rx=\"1.5\" fill=\"{}\"/>",
                    palette.accent
                ));
            }
        } else {
            let x = if variant.is_some() || appearance == Appearance::Light {
                0
            } else {
                100
            };
            let width = if variant.is_some() { 200 } else { 100 };
            let label = if appearance == Appearance::Light {
                "No light variant"
            } else {
                "No dark variant"
            };
            result.push_str(&format!(r##"<rect x="{x}" width="{width}" height="126" fill="url(#missing)"/><text x="{}" y="68" text-anchor="middle" font-family="sans-serif" font-size="9" fill="#353B45">{label}</text>"##, x + width / 2));
        }
        result.push_str("</g>");
    }
    if selected {
        let p = family
            .palette(active.unwrap_or(Appearance::Dark))
            .or(family.dark.as_ref())
            .or(family.light.as_ref())
            .unwrap();
        let foreground = ferese_theme_model::hex(ferese_theme_model::readable(
            [1.; 4],
            ferese_theme_model::rgba(&p.accent).unwrap(),
            4.5,
        ));
        result.push_str(&format!(r#"<circle cx="183" cy="16" r="10" fill="{}"/><path d="M178 16l3 3 6-6" fill="none" stroke="{foreground}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>"#, p.accent));
    }
    result.push_str("</g></svg>");
    result
}

pub fn neighbor(index: usize, count: usize, key: &keyboard::key::Named) -> Option<usize> {
    use keyboard::key::Named::*;
    if count == 0 {
        return None;
    }
    Some(match key {
        ArrowRight => (index + 1) % count,
        ArrowLeft => (index + count - 1) % count,
        ArrowDown => {
            if index + 3 < count {
                index + 3
            } else {
                index % 3
            }
        }
        ArrowUp => {
            if index >= 3 {
                index - 3
            } else {
                ((count - 1 - index) / 3) * 3 + index
            }
        }
        Enter => index,
        Home => 0,
        End => count - 1,
        _ => return None,
    })
}

#[derive(Default)]
struct Focus(bool);
impl cosmic::iced::advanced::widget::operation::Focusable for Focus {
    fn is_focused(&self) -> bool {
        self.0
    }
    fn focus(&mut self) {
        self.0 = true;
    }
    fn unfocus(&mut self) {
        self.0 = false;
    }
}

type Navigation<'a, M> = Box<dyn Fn(&keyboard::key::Named) -> Option<M> + 'a>;

pub struct Radio<'a, M> {
    content: Element<'a, M>,
    id: Id,
    selected: bool,
    group: bool,
    label: String,
    navigate: Option<Navigation<'a, M>>,
}

pub fn radio<'a, M: Clone + 'a>(
    content: impl Into<Element<'a, M>>,
    id: Id,
    selected: bool,
    navigate: impl Fn(&keyboard::key::Named) -> Option<M> + 'a,
) -> Element<'a, M> {
    Element::new(Radio {
        content: content.into(),
        id,
        selected,
        group: false,
        label: String::new(),
        navigate: Some(Box::new(navigate)),
    })
}

pub fn group<'a, M: Clone + 'a>(content: impl Into<Element<'a, M>>, id: Id, label: &str) -> Element<'a, M> {
    Element::new(Radio {
        content: content.into(),
        id,
        selected: false,
        group: true,
        label: label.into(),
        navigate: None,
    })
}

impl<M: Clone> Widget<M, Theme, Renderer> for Radio<'_, M> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Focus>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(Focus::default())
    }
    fn children(&self) -> Vec<Tree> {
        vec![Tree::new(&self.content)]
    }
    fn diff(&mut self, tree: &mut Tree) {
        tree.diff_children(std::slice::from_mut(&mut self.content));
    }
    fn size(&self) -> Size<Length> {
        self.content.as_widget().size()
    }
    fn layout(&mut self, tree: &mut Tree, renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        self.content
            .as_widget_mut()
            .layout(&mut tree.children[0], renderer, limits)
    }
    fn operate(&mut self, tree: &mut Tree, layout: Layout<'_>, renderer: &Renderer, operation: &mut dyn Operation<()>) {
        if self.group {
            self.content
                .as_widget_mut()
                .operate(&mut tree.children[0], layout, renderer, operation);
        } else if self.selected {
            operation.focusable(Some(&self.id), layout.bounds(), tree.state.downcast_mut::<Focus>());
        } else {
            tree.state.downcast_mut::<Focus>().0 = false;
        }
    }
    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, M>,
        viewport: &Rectangle,
    ) {
        if !self.group {
            if let Event::A11y(id, request) = event {
                if !iced_accessibility::IdEq::eq(&id.0, &self.id.0) {
                    return;
                }
                if matches!(request.action, accesskit::Action::Focus | accesskit::Action::Click)
                    && let Some(message) = self
                        .navigate
                        .as_ref()
                        .and_then(|navigate| navigate(&keyboard::key::Named::Enter))
                {
                    shell.publish(message);
                    shell.capture_event();
                    return;
                }
            }
            if matches!(event, Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))) {
                tree.state.downcast_mut::<Focus>().0 = self.selected && cursor.is_over(layout.bounds());
            }
        }
        if !self.group && self.selected && tree.state.downcast_ref::<Focus>().0 {
            if let Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Character(key),
                ..
            }) = event
                && key.as_str() == " "
                && let Some(message) = self
                    .navigate
                    .as_ref()
                    .and_then(|navigate| navigate(&keyboard::key::Named::Enter))
            {
                shell.publish(message);
                shell.capture_event();
                return;
            }
            if let Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(key),
                ..
            }) = event
                && let Some(message) = self.navigate.as_ref().and_then(|navigate| navigate(key))
            {
                shell.publish(message);
                shell.capture_event();
                return;
            }
        }
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
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        self.content
            .as_widget()
            .draw(&tree.children[0], renderer, theme, style, layout, cursor, viewport);
        if !self.group && self.selected && tree.state.downcast_ref::<Focus>().0 {
            use renderer::Renderer as _;
            renderer.fill_quad(
                renderer::Quad {
                    bounds: layout.bounds(),
                    border: cosmic::iced::Border {
                        shape: BorderShape::Continuous,
                        color: style.text_color,
                        width: 2.,
                        radius: 10.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                cosmic::iced::Color::TRANSPARENT,
            );
        }
    }
    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.content
            .as_widget()
            .mouse_interaction(&tree.children[0], layout, cursor, viewport, renderer)
    }
    fn a11y_nodes(&self, layout: Layout<'_>, tree: &Tree, cursor: mouse::Cursor) -> A11yTree {
        let children = self.content.as_widget().a11y_nodes(layout, &tree.children[0], cursor);
        if self.group {
            let mut node = accesskit::Node::new(accesskit::Role::RadioGroup);
            node.set_label(self.label.clone());
            A11yTree::node_with_child_tree(iced_accessibility::A11yNode::new(node, self.id.clone()), children)
        } else {
            let mut children = children;
            for node in children.root_mut() {
                node.node_mut().set_role(accesskit::Role::RadioButton);
                node.node_mut().set_toggled(if self.selected {
                    accesskit::Toggled::True
                } else {
                    accesskit::Toggled::False
                });
            }
            children
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arrow_navigation_wraps_rows_and_columns() {
        use keyboard::key::Named::*;
        assert_eq!(neighbor(0, 6, &ArrowLeft), Some(5));
        assert_eq!(neighbor(1, 6, &ArrowUp), Some(4));
        assert_eq!(neighbor(4, 6, &ArrowDown), Some(1));
        assert_eq!(neighbor(5, 6, &Home), Some(0));
        assert_eq!(neighbor(0, 7, &End), Some(6));
    }
    #[test]
    fn paired_previews_share_one_window_across_the_seam() {
        let family = ferese_config::families::builtins().remove(1);
        let svg = svg(&family, None, Some(Appearance::Dark), true);
        assert_eq!(svg.matches("x=\"24\" y=\"35\"").count(), 2);
        assert!(svg.contains("clip-path=\"url(#light)\""));
        assert!(svg.contains("clip-path=\"url(#dark)\""));
    }
}
