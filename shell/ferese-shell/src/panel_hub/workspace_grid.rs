mod drag;

use cosmic::app::Task;
use cosmic::iced::advanced::widget::{Id, operation};
use cosmic::iced::{Alignment, Length, keyboard::key::Named, window};
use cosmic::widget::{button, column, container, mouse_area, row, scrollable};
use cosmic::{Element, theme};

use crate::control::{WindowSnapshot, WorkspaceSnapshot};
use crate::{FereseShell, Message, motion, text};

const TILE_WIDTH: f32 = 56.;
const TILE_HEIGHT: f32 = 40.;
const GAP: f32 = 8.;

fn tiles(mut workspaces: Vec<&WorkspaceSnapshot>, active: u64) -> Vec<&WorkspaceSnapshot> {
    workspaces.sort_by_key(|workspace| (workspace.index, workspace.id));
    let empty = workspaces
        .iter()
        .find(|w| w.window_count == 0 && w.id != active)
        .map(|w| w.id);
    workspaces.retain(|w| w.window_count > 0 || w.id == active || Some(w.id) == empty);
    workspaces
}

fn columns(width: f32) -> usize {
    (((width + GAP) / (TILE_WIDTH + GAP)).floor() as usize).clamp(1, 5)
}

fn caption(workspace: &WorkspaceSnapshot) -> String {
    let name = if workspace.name.is_empty() || workspace.name == workspace.index.to_string() {
        workspace.index.to_string()
    } else {
        format!("{} · {}", workspace.index, workspace.name)
    };
    format!(
        "{name} — {} {}",
        workspace.window_count,
        if workspace.window_count == 1 {
            "window"
        } else {
            "windows"
        }
    )
}

fn tile_id(bar: window::Id, workspace: u64) -> Id {
    Id::new(format!("morph-workspace-{bar:?}-{workspace}"))
}

struct FocusedWorkspace {
    tiles: Vec<(Id, u64)>,
    focused: Option<u64>,
}

impl cosmic::iced::advanced::widget::Operation<Option<u64>> for FocusedWorkspace {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn cosmic::iced::advanced::widget::Operation<Option<u64>>)) {
        operate(self);
    }

    fn focusable(&mut self, id: Option<&Id>, _: cosmic::iced::Rectangle, state: &mut dyn operation::Focusable) {
        if state.is_focused() {
            self.focused = self
                .tiles
                .iter()
                .find(|(tile, _)| Some(tile) == id)
                .map(|(_, workspace)| *workspace)
                .or(self.focused);
        }
    }

    fn finish(&self) -> operation::Outcome<Option<u64>> {
        operation::Outcome::Some(self.focused)
    }
}

// Fit the actual rectangles together, preserving aspect and relative positions.
fn mini_rects(windows: &[&WindowSnapshot]) -> Vec<(u64, cosmic::iced::Rectangle)> {
    let rectangles: Vec<_> = windows.iter().filter_map(|w| w.geometry.map(|r| (w.id, r))).collect();
    if rectangles.is_empty() {
        return Vec::new();
    }
    let x = rectangles.iter().map(|(_, r)| r[0]).fold(f64::INFINITY, f64::min);
    let y = rectangles.iter().map(|(_, r)| r[1]).fold(f64::INFINITY, f64::min);
    let right = rectangles
        .iter()
        .map(|(_, r)| r[0] + r[2])
        .fold(f64::NEG_INFINITY, f64::max);
    let bottom = rectangles
        .iter()
        .map(|(_, r)| r[1] + r[3])
        .fold(f64::NEG_INFINITY, f64::max);
    let scale = (44. / (right - x)).min(17. / (bottom - y));
    let dx = (44. - (right - x) * scale) / 2.;
    let dy = (17. - (bottom - y) * scale) / 2.;
    rectangles
        .into_iter()
        .map(|(id, r)| {
            (
                id,
                cosmic::iced::Rectangle {
                    x: (dx + (r[0] - x) * scale + 0.3) as f32,
                    y: (dy + (r[1] - y) * scale + 0.3) as f32,
                    width: (r[2] * scale - 0.6).max(0.2) as f32,
                    height: (r[3] * scale - 0.6).max(0.2) as f32,
                },
            )
        })
        .collect()
}

fn miniature(windows: &[&WindowSnapshot], color: cosmic::iced::Color) -> cosmic::widget::Svg<'static, cosmic::Theme> {
    let mut svg = String::from("<svg xmlns='http://www.w3.org/2000/svg' width='44' height='17' viewBox='0 0 44 17'>");
    for (_, r) in mini_rects(windows) {
        use std::fmt::Write;
        let _ = write!(
            svg,
            "<rect x='{}' y='{}' width='{}' height='{}' rx='0.8' fill='rgb({},{},{})' fill-opacity='{}'/>",
            r.x,
            r.y,
            r.width,
            r.height,
            (color.r * 255.) as u8,
            (color.g * 255.) as u8,
            (color.b * 255.) as u8,
            color.a
        );
    }
    svg.push_str("</svg>");
    cosmic::widget::svg(cosmic::widget::svg::Handle::from_memory(svg.into_bytes()))
        .width(44)
        .height(17)
}

impl FereseShell {
    pub(crate) fn hub_workspace_move(&mut self, id: window::Id, window: u64, destination: u64) -> Task<Message> {
        let output = self.output_for_bar(id).map(|o| o.id);
        let source = self
            .snapshot
            .windows
            .iter()
            .find(|w| w.id == window)
            .map(|w| w.workspace);
        let valid = |workspace| self.snapshot.workspaces_for_output(output).any(|w| w.id == workspace);
        if source.is_some_and(valid)
            && valid(destination)
            && let Some(control) = &self.control
        {
            control.move_window_to_workspace(window, destination);
        }
        Task::none()
    }

    pub(crate) fn hub_workspace_hover(&mut self, id: window::Id, workspace: Option<u64>) -> Task<Message> {
        if let Some(runtime) = self
            .outputs
            .iter()
            .find(|entry| entry.bar == id)
            .and_then(|entry| entry.runtime.hub.as_ref())
        {
            let mut runtime = runtime.borrow_mut();
            runtime.workspace_hover = workspace;
            runtime.workspace_selected = None;
        }
        Task::none()
    }

    pub(crate) fn hub_workspace_select(&mut self, id: window::Id, workspace: u64) -> Task<Message> {
        let valid = self
            .snapshot
            .workspaces_for_output(self.output_for_bar(id).map(|o| o.id))
            .any(|w| w.id == workspace);
        if !valid {
            return Task::none();
        }
        if let Some(control) = &self.control {
            control.activate_workspace(workspace);
        }
        if let Some(runtime) = self
            .outputs
            .iter()
            .find(|entry| entry.bar == id)
            .and_then(|entry| entry.runtime.hub.as_ref())
        {
            runtime.borrow_mut().workspace_selection_pending = Some(workspace);
        }
        self.close_hub(id)
    }

    pub(crate) fn hub_workspace_key(&self, id: window::Id, key: Named) -> Task<Message> {
        self.query_hub_workspace_focus(id, Some(key))
    }

    pub(crate) fn query_hub_workspace_focus(&self, id: window::Id, key: Option<Named>) -> Task<Message> {
        let output = self.output_for_bar(id);
        let tiles = tiles(
            self.snapshot.workspaces_for_output(output.map(|o| o.id)).collect(),
            output.map_or(0, |o| o.active_workspace),
        )
        .into_iter()
        .map(|workspace| (tile_id(id, workspace.id), workspace.id))
        .collect();
        cosmic::iced::advanced::widget::operate(FocusedWorkspace { tiles, focused: None })
            .map(move |focused| cosmic::Action::App(Message::HubWorkspaceFocus(id, focused, key)))
    }

    pub(crate) fn hub_workspace_focus(
        &mut self,
        id: window::Id,
        focused: Option<u64>,
        key: Option<Named>,
    ) -> Task<Message> {
        if self.hub_mode(id)
            != Some(ferese_panel::morph::Mode::Expanded(
                ferese_panel::morph::Content::Workspaces,
            ))
        {
            return Task::none();
        }
        let Some(output) = self.output_for_bar(id) else {
            return Task::none();
        };
        let workspaces = tiles(
            self.snapshot.workspaces_for_output(Some(output.id)).collect(),
            output.active_workspace,
        );
        let Some(runtime) = self
            .outputs
            .iter()
            .find(|e| e.bar == id)
            .and_then(|e| e.runtime.hub.as_ref())
        else {
            return Task::none();
        };
        let mut runtime = runtime.borrow_mut();
        runtime.workspace_selected = focused;
        let Some(key) = key else {
            return Task::none();
        };
        let selected = focused.unwrap_or(output.active_workspace);
        let current = workspaces.iter().position(|w| w.id == selected).unwrap_or(0);
        let cols = columns((runtime.hub.target_size()[0] - 28.) as f32);
        let next = match key {
            Named::ArrowLeft => current.saturating_sub(1),
            Named::ArrowRight => (current + 1).min(workspaces.len().saturating_sub(1)),
            Named::ArrowUp => current.saturating_sub(cols),
            Named::ArrowDown => (current + cols).min(workspaces.len().saturating_sub(1)),
            _ => return Task::none(),
        };
        let Some(workspace) = workspaces.get(next) else {
            return Task::none();
        };
        runtime.workspace_selected = Some(workspace.id);
        runtime.workspace_hover = None;
        let rows = workspaces.len().div_ceil(cols);
        Task::batch([
            cosmic::iced::advanced::widget::operate(operation::focusable::focus(tile_id(id, workspace.id))),
            cosmic::iced::widget::scrollable::snap_to(
                Id::new(format!("morph-workspace-grid-{id:?}")),
                cosmic::iced::widget::scrollable::RelativeOffset {
                    x: None,
                    y: Some(if rows > 2 {
                        (next / cols).saturating_sub(1) as f32 / (rows - 2) as f32
                    } else {
                        0.
                    }),
                },
            ),
        ])
    }

    pub(crate) fn hub_workspace_grid(&self, id: window::Id, opacity: f32) -> Element<'_, cosmic::Action<Message>> {
        let output = self.output_for_bar(id);
        let active = output.map_or(0, |o| o.active_workspace);
        let workspaces = tiles(
            self.snapshot.workspaces_for_output(output.map(|o| o.id)).collect(),
            active,
        );
        let runtime = self
            .outputs
            .iter()
            .find(|e| e.bar == id)
            .and_then(|e| e.runtime.hub.as_ref())
            .map(|r| r.borrow());
        let width = runtime
            .as_ref()
            .map_or(312., |r| (r.hub.target_size()[0] - 28.) as f32)
            .max(1.);
        let cols = columns(width);
        let hovered = runtime
            .as_ref()
            .and_then(|r| r.workspace_hover.or(r.workspace_selected))
            .unwrap_or(active);
        let current = workspaces
            .iter()
            .find(|w| w.id == hovered)
            .or_else(|| workspaces.iter().find(|w| w.id == active));
        let mut label = current.map_or_else(|| "No workspaces".to_owned(), |w| caption(w));
        if let Some(workspace) = current.filter(|_| runtime.as_ref().is_some_and(|r| r.workspace_hover.is_some())) {
            let titles = self
                .snapshot
                .windows
                .iter()
                .filter(|w| w.workspace == workspace.id && !w.title.is_empty())
                .map(|w| w.title.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            if !titles.is_empty() {
                label.push_str(&format!(" · {titles}"));
            }
        }
        let palette = self.config.theme.palette();
        let mut targets = Vec::new();
        let mut grid = column::with_capacity(workspaces.len().div_ceil(cols)).spacing(GAP);
        for group in workspaces.chunks(cols) {
            let mut line = row::with_capacity(cols).spacing(GAP);
            for workspace in group {
                let workspace_id = workspace.id;
                let index = workspace.index;
                let windows: Vec<_> = self
                    .snapshot
                    .windows
                    .iter()
                    .filter(|w| w.workspace == workspace_id)
                    .collect();
                let index_in_grid = targets.len();
                let origin = cosmic::iced::Vector::new(
                    (index_in_grid % cols) as f32 * (TILE_WIDTH + GAP),
                    (index_in_grid / cols) as f32 * (TILE_HEIGHT + GAP),
                );
                targets.push(drag::Target {
                    workspace: workspace_id,
                    bounds: cosmic::iced::Rectangle::new(
                        cosmic::iced::Point::ORIGIN + origin,
                        cosmic::iced::Size::new(TILE_WIDTH.min(width), TILE_HEIGHT),
                    ),
                    windows: mini_rects(&windows)
                        .into_iter()
                        .map(|(id, rect)| (id, rect + origin + cosmic::iced::Vector::new(6., 19.)))
                        .collect(),
                });
                let urgent = workspace_id != active && windows.iter().any(|w| w.urgent);
                let empty = workspace.window_count == 0 && workspace_id != active;
                let label = format!(
                    "{}{}{}",
                    caption(workspace),
                    if workspace_id == active { ", active" } else { "" },
                    windows.iter().map(|w| format!(", {}", w.title)).collect::<String>()
                );
                let transition = self.workspace_ui.transition(workspace_id, workspace_id == active);
                line = line.push(motion::geometry_driven(
                    transition.revision(),
                    move |now| {
                        let progress = transition.progress(now).clamp(0., 1.);
                        let alpha = opacity * if empty { 0.6 } else { 1. };
                        let foreground = ferese_theme::mix(palette.muted, palette.accent, progress).scale_alpha(alpha);
                        let fill = cosmic::iced::Color {
                            a: (0.055 + 0.105 * progress) * alpha,
                            ..ferese_theme::mix(palette.text, palette.accent, progress)
                        };
                        let appearance = button::Style {
                            background: Some(fill.into()),
                            border_radius: 10.0.into(),
                            border_width: 0.5,
                            border_color: palette.accent.scale_alpha(progress * alpha * 0.5),
                            text_color: Some(foreground),
                            ..Default::default()
                        };
                        let mut heading = row![
                            text(index.to_string())
                                .size(11)
                                .class(theme::Text::Color(foreground))
                                .width(Length::Fill)
                        ]
                        .align_y(Alignment::Center);
                        if urgent {
                            let pulse = if motion::settings().motion_enabled() {
                                0.65 + 0.35
                                    * (now
                                        .saturating_duration_since(transition.revision().unwrap_or(now))
                                        .as_secs_f32()
                                        * 4.)
                                        .cos()
                            } else {
                                1.
                            };
                            heading = heading.push(container(cosmic::widget::Space::new()).width(4).height(4).class(
                                theme::Container::custom(move |_| container::Style {
                                    background: Some(palette.error.scale_alpha(alpha * pulse).into()),
                                    border: cosmic::iced::Border {
                                        radius: 2.0.into(),
                                        ..Default::default()
                                    },
                                    ..Default::default()
                                }),
                            ));
                        }
                        let content =
                            column![container(heading).height(14), miniature(&windows, foreground)].spacing(1);
                        mouse_area(
                            button::custom(content)
                                .id(tile_id(id, workspace_id))
                                .name(label.clone())
                                .width(TILE_WIDTH.min(width))
                                .height(TILE_HEIGHT)
                                .padding([4, 6])
                                .class(theme::Button::Custom {
                                    active: Box::new(move |focused, _| button::Style {
                                        outline_width: if focused { 1. } else { 0. },
                                        outline_color: palette.accent.scale_alpha(alpha),
                                        ..appearance
                                    }),
                                    hovered: Box::new(move |_, _| button::Style {
                                        background: Some(palette.accent.scale_alpha(0.12 * alpha).into()),
                                        ..appearance
                                    }),
                                    pressed: Box::new(move |_, _| appearance),
                                    disabled: Box::new(move |_| appearance),
                                })
                                .on_press(cosmic::Action::App(Message::HubWorkspaceSelect(id, workspace_id))),
                        )
                        .on_enter(cosmic::Action::App(Message::HubWorkspaceHover(id, Some(workspace_id))))
                        .on_exit(cosmic::Action::App(Message::HubWorkspaceHover(id, None)))
                        .into()
                    },
                    move |now| transition.active(now) || (urgent && motion::settings().motion_enabled()),
                    |_| {},
                    |_| None,
                    None,
                ));
            }
            grid = grid.push(line);
        }
        column![
            scrollable(drag::area(grid.into(), id, targets))
                .id(Id::new(format!("morph-workspace-grid-{id:?}")))
                .height(88)
                .direction(cosmic::iced::widget::scrollable::Direction::Vertical(
                    cosmic::iced::widget::scrollable::Scrollbar::default()
                        .width(0)
                        .scroller_width(0)
                )),
            text(label)
                .size(12)
                .class(theme::Text::Color(palette.muted.scale_alpha(opacity)))
                .width(Length::Fill)
                .wrapping(cosmic::iced::widget::text::Wrapping::None),
        ]
        .spacing(8)
        .width(Length::Fill)
        .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(id: u64, count: u32) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            id,
            index: id as u32,
            name: id.to_string(),
            output: Some(1),
            window_count: count,
            visible: false,
            focused: false,
        }
    }

    #[test]
    fn grid_keeps_occupied_active_and_one_empty_destination_without_a_ten_tile_cap() {
        let mut all: Vec<_> = (1..=14).map(|id| workspace(id, u32::from(id < 12))).collect();
        all.reverse();
        let shown = tiles(all.iter().collect(), 13);
        assert_eq!(
            shown.iter().map(|w| w.id).collect::<Vec<_>>(),
            (1..=13).collect::<Vec<_>>()
        );
        assert_eq!(columns(312.), 5);
        assert_eq!(columns(212.), 3);
        assert_eq!(caption(&workspace(3, 1)), "3 — 1 window");
        let mut named = workspace(3, 2);
        named.name = "Chat".into();
        assert_eq!(caption(&named), "3 · Chat — 2 windows");
    }

    #[test]
    fn arrows_follow_the_tile_focused_by_tab_in_both_directions_and_narrow_grids() {
        use cosmic::iced::Size;
        use cosmic::iced::advanced::{Layout, layout, renderer::Headless, widget};
        let renderer = cosmic::iced::futures::executor::block_on(cosmic::Renderer::new(
            cosmic::iced::Font::default(),
            cosmic::iced::Pixels(14.),
            Some("tiny-skia"),
        ))
        .unwrap();
        for (width, start, reverse, key, expected) in [
            (340., 4, false, Named::ArrowDown, 10),
            (340., 8, true, Named::ArrowLeft, 6),
            (240., 4, false, Named::ArrowDown, 8),
        ] {
            let mut shell = crate::tests::shell_with_measured_panel();
            shell.config.panels[0].style = ferese_panel::PanelStyle::Morph;
            shell.config.panels[0].morph.expanded_width = width;
            shell.outputs[0]
                .runtime
                .reset(&shell.config.panels[0], &shell.snapshot, Some("test"));
            let bar = shell.outputs[0].bar;
            shell.snapshot.outputs = vec![crate::control::OutputSnapshot {
                id: 1,
                name: "test".into(),
                active_workspace: 1,
                focused: true,
            }];
            shell.snapshot.workspaces = (1..=12).map(|id| workspace(id, 1)).collect();
            drop(shell.open_hub_overview(bar));
            shell.outputs[0]
                .runtime
                .hub
                .as_ref()
                .unwrap()
                .borrow_mut()
                .workspace_selected = Some(1);
            let focused = {
                let mut view = shell.hub_workspace_grid(bar, 1.);
                let mut tree = widget::Tree::new(&view);
                let node = view.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, Size::new(width as f32 - 28., 128.)),
                );
                view.as_widget_mut().operate(
                    &mut tree,
                    Layout::new(&node),
                    &renderer,
                    &mut operation::focusable::focus::<()>(tile_id(bar, start)),
                );
                let mut tab: Box<dyn widget::Operation> = if reverse {
                    Box::new(operation::focusable::focus_previous())
                } else {
                    Box::new(operation::focusable::focus_next())
                };
                loop {
                    view.as_widget_mut()
                        .operate(&mut tree, Layout::new(&node), &renderer, &mut tab);
                    match tab.finish() {
                        operation::Outcome::Chain(next) => tab = next,
                        _ => break,
                    }
                }
                let mut query = FocusedWorkspace {
                    tiles: (1..=12).map(|id| (tile_id(bar, id), id)).collect(),
                    focused: None,
                };
                view.as_widget_mut().operate(
                    &mut tree,
                    Layout::new(&node),
                    &renderer,
                    &mut operation::black_box(&mut query),
                );
                query.focused
            };
            assert_eq!(focused, Some(if reverse { start - 1 } else { start + 1 }));
            drop(shell.hub_workspace_focus(bar, focused, None));
            assert_eq!(
                shell.outputs[0]
                    .runtime
                    .hub
                    .as_ref()
                    .unwrap()
                    .borrow()
                    .workspace_selected,
                focused
            );
            drop(shell.hub_workspace_focus(bar, focused, Some(key)));
            assert_eq!(
                shell.outputs[0]
                    .runtime
                    .hub
                    .as_ref()
                    .unwrap()
                    .borrow()
                    .workspace_selected,
                Some(expected)
            );
        }
    }

    #[test]
    fn miniatures_preserve_real_layout_and_do_not_invent_missing_windows() {
        let window = |id, rect| WindowSnapshot {
            id,
            workspace: 1,
            app_id: "test".into(),
            title: "test".into(),
            geometry: rect,
            focused: false,
            urgent: false,
            fullscreen: false,
            maximized: false,
            floating: false,
        };
        let a = window(1, Some([-400., 20., 300., 200.]));
        let b = window(2, Some([-100., 20., 100., 200.]));
        let c = window(3, None);
        let rectangles = mini_rects(&[&a, &b, &c]);
        assert_eq!(rectangles.len(), 2);
        assert_eq!(rectangles[0].0, 1);
        let left = rectangles[0].1;
        let right = rectangles[1].1;
        assert!((left.height - right.height).abs() < 0.001);
        assert!(left.width > right.width * 2.9);
        assert!(left.x + left.width < right.x);
        assert!(right.x + right.width <= 44. && left.y + left.height <= 17.);
    }
}
