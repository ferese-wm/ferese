use std::time::{Duration, Instant};

use cosmic::app::Task;
use cosmic::iced::{Alignment, mouse::ScrollDelta, window};
use cosmic::widget::{button, container, row};
use cosmic::{Element, theme};
use ferese_panel::{ItemKind, morph::Mode};

use super::Peek;
use crate::control::WorkspaceSnapshot;
use crate::{FereseShell, Message, color_with_opacity, motion, text};

const SLOT: f32 = 12.;
const ACTIVE_EXTRA: f32 = 12.;
const DOT_INSET: f32 = 3.;
const LIMIT: usize = 4;

struct Selection<'a> {
    workspaces: Vec<&'a WorkspaceSnapshot>,
    hidden: usize,
}

impl Selection<'_> {
    fn width(&self) -> f32 {
        if self.workspaces.is_empty() {
            return 0.;
        }
        // Six-pixel gaps, room for the active pill, and an optional overflow count.
        self.workspaces.len() as f32 * SLOT + ACTIVE_EXTRA - DOT_INSET
            + if self.hidden > 0 {
                6. + (self.hidden.to_string().len() + 1) as f32 * 7.
            } else {
                0.
            }
    }
}

fn select(mut workspaces: Vec<&WorkspaceSnapshot>, active: u64, budget: f32) -> Selection<'_> {
    workspaces.retain(|workspace| workspace.window_count > 0 || workspace.id == active);
    workspaces.sort_by_key(|workspace| (workspace.index, workspace.id));
    let count = workspaces.len();
    if count == 0 || (count <= LIMIT && count as f32 * SLOT + ACTIVE_EXTRA - DOT_INSET <= budget) {
        return Selection { workspaces, hidden: 0 };
    }
    let overflow_width = 6. + (count.to_string().len() + 1) as f32 * 7.;
    let visible = (((budget - overflow_width - ACTIVE_EXTRA + DOT_INSET) / SLOT)
        .floor()
        .max(1.) as usize)
        .min(LIMIT)
        .min(count);
    let position = workspaces
        .iter()
        .position(|workspace| workspace.id == active)
        .unwrap_or(0);
    let start = position.saturating_sub(visible / 2).min(count - visible);
    Selection {
        workspaces: workspaces[start..start + visible].to_vec(),
        hidden: count - visible,
    }
}

#[derive(Default)]
pub(super) struct Scroll {
    amount: f32,
    pixels: bool,
    last_input: Option<Instant>,
    last_switch: Option<Instant>,
}

impl Scroll {
    fn step(&mut self, delta: ScrollDelta, now: Instant) -> Option<i32> {
        let (x, y, pixels) = match delta {
            ScrollDelta::Lines { x, y } => (x, y, false),
            ScrollDelta::Pixels { x, y } => (x, y, true),
        };
        let amount = if y.abs() >= x.abs() { y } else { -x };
        if !amount.is_finite() || amount == 0. {
            return None;
        }
        if pixels != self.pixels
            || self
                .last_input
                .is_none_or(|last| now.saturating_duration_since(last) > Duration::from_millis(250))
            || self.amount.signum() != amount.signum()
        {
            self.amount = 0.;
        }
        self.pixels = pixels;
        self.last_input = Some(now);
        if self
            .last_switch
            .is_some_and(|last| now.saturating_duration_since(last) < Duration::from_millis(120))
        {
            return None;
        }
        self.amount += amount;
        let threshold = if pixels { 40. } else { 1. };
        if self.amount.abs() < threshold {
            return None;
        }
        let direction = if self.amount > 0. { -1 } else { 1 };
        self.amount = 0.;
        self.last_switch = Some(now);
        Some(direction)
    }
}

impl FereseShell {
    fn hub_workspace_selection(&self, id: window::Id) -> Selection<'_> {
        let panel = self.panel_for_bar(id);
        let enabled = [&panel.start, &panel.center, &panel.end]
            .into_iter()
            .flat_map(|zone| &zone.groups)
            .flat_map(|group| &group.items)
            .any(|item| item.visible && matches!(item.kind, ItemKind::Workspaces { .. }));
        let Some(output) = self.output_for_bar(id).filter(|_| enabled) else {
            return Selection {
                workspaces: Vec::new(),
                hidden: 0,
            };
        };
        let available = self
            .outputs
            .iter()
            .find(|entry| entry.bar == id)
            .and_then(|entry| entry.size)
            .map_or(panel.morph.expanded_width, |(width, _)| {
                (f64::from(width) - 2. * f64::from(panel.resolved_geometry().side_margins)).max(0.)
            });
        select(
            self.snapshot.workspaces_for_output(Some(output.id)).collect(),
            output.active_workspace,
            // Reserve the four panel tabs and Close before allocating workspace dots.
            (panel.morph.expanded_width.min(available) - 28. - 5. * 32. - 4.).max(0.) as f32,
        )
    }

    pub(super) fn hub_workspace_width(&self, id: window::Id) -> f64 {
        f64::from(self.hub_workspace_selection(id).width())
    }

    pub(crate) fn sync_hub_workspaces(&mut self) -> Task<Message> {
        let changes: Vec<_> = self
            .outputs
            .iter()
            .filter_map(|entry| {
                let runtime = entry.runtime.hub.as_ref()?;
                let width = self.hub_workspace_width(entry.bar);
                let width = if width > 0. { width + 10. } else { 0. };
                let mut runtime = runtime.borrow_mut();
                if runtime.observed_workspace.is_none() {
                    runtime.observed_workspace = self.output_for_bar(entry.bar).map(|output| output.active_workspace);
                }
                if runtime.workspace_width == width {
                    return None;
                }
                runtime.workspace_width = width;
                matches!(runtime.hub.mode(), Mode::Idle).then_some((entry.bar, runtime.hub.mode(), runtime.menu))
            })
            .collect();
        Task::batch(
            changes
                .into_iter()
                .map(|(id, mode, menu)| self.set_hub_mode(id, mode, menu)),
        )
    }

    pub(crate) fn open_hub_overview(&mut self, id: window::Id) -> Task<Message> {
        if self.bar_hidden(id) || self.hub_mode(id).is_none() {
            return Task::none();
        }
        if let Some(control) = &self.control {
            if let Some(output) = self.output_for_bar(id) {
                control.activate_workspace(output.active_workspace);
            }
            control.set_overview_active(true);
        }
        self.overview_active = true;
        self.close_hub(id)
    }

    pub(crate) fn sync_hub_workspace_feedback(&mut self, suppressed: bool) -> Task<Message> {
        let changed: Vec<_> = self
            .outputs
            .iter()
            .filter_map(|entry| {
                let active = self.output_for_bar(entry.bar)?.active_workspace;
                let mut runtime = entry.runtime.hub.as_ref()?.borrow_mut();
                let previous = runtime.observed_workspace.replace(active);
                (!suppressed
                    && previous.is_some_and(|previous| previous != active)
                    && matches!(runtime.hub.mode(), Mode::Idle | Mode::Peek))
                .then_some(entry.bar)
            })
            .collect();
        Task::batch(changed.into_iter().map(|id| self.show_hub_workspace_feedback(id)))
    }

    fn show_hub_workspace_feedback(&mut self, id: window::Id) -> Task<Message> {
        if self.bar_hidden(id) || self.hub_workspace_width(id) == 0. {
            return Task::none();
        }
        let Some(runtime) = self
            .outputs
            .iter()
            .find(|entry| entry.bar == id)
            .and_then(|entry| entry.runtime.hub.as_ref())
        else {
            return Task::none();
        };
        let revision = Instant::now();
        let first = {
            let mut runtime = runtime.borrow_mut();
            let first = runtime.workspace_feedback.is_none();
            if first {
                runtime.previous_peek = runtime.peek;
                runtime.previous_peek_width = runtime.peek_width;
                runtime.peek = Peek::Workspaces;
                runtime.peek_width = 260.;
                if runtime.hub.mode() == Mode::Peek {
                    runtime.hub.replace_content();
                }
            }
            runtime.workspace_feedback = Some(revision);
            first
        };
        let policy = motion::settings();
        let fade = if first && policy.motion_enabled() {
            0.25 / policy.speed
        } else {
            0.
        };
        let timeout = Duration::from_secs_f64(1.2 + fade);
        Task::batch([
            self.set_hub_mode(id, Mode::Peek, None),
            Task::perform(
                async move {
                    tokio::time::sleep(timeout).await;
                },
                move |_| cosmic::Action::App(Message::HubWorkspaceFeedbackExpired(id, revision)),
            ),
        ])
    }

    pub(crate) fn hub_workspace_feedback_expired(&mut self, id: window::Id, revision: Instant) -> Task<Message> {
        let Some(runtime) = self
            .outputs
            .iter()
            .find(|entry| entry.bar == id)
            .and_then(|entry| entry.runtime.hub.as_ref())
        else {
            return Task::none();
        };
        let hovered = {
            let mut runtime = runtime.borrow_mut();
            if runtime.workspace_feedback != Some(revision) || runtime.hub.mode() != Mode::Peek {
                return Task::none();
            }
            runtime.workspace_feedback = None;
            let previous = runtime.previous_peek;
            runtime.previous_peek = Peek::Workspaces;
            runtime.peek = previous;
            runtime.peek_width = runtime.previous_peek_width;
            if runtime.hovered {
                runtime.hub.replace_content();
            }
            runtime.hovered
        };
        self.set_hub_mode(id, if hovered { Mode::Peek } else { Mode::Idle }, None)
    }

    pub(crate) fn scroll_hub_workspaces(&mut self, id: window::Id, delta: ScrollDelta) -> Task<Message> {
        if self.bar_hidden(id) || self.hub_workspace_width(id) == 0. {
            return Task::none();
        }
        let Some(runtime) = self
            .outputs
            .iter()
            .find(|entry| entry.bar == id)
            .and_then(|entry| entry.runtime.hub.as_ref())
        else {
            return Task::none();
        };
        let Some(direction) = runtime.borrow_mut().workspace_scroll.step(delta, Instant::now()) else {
            return Task::none();
        };
        let Some(output) = self.output_for_bar(id) else {
            return Task::none();
        };
        // Include the trailing empty workspace so scrolling can create a new working space.
        let mut workspaces: Vec<_> = self.snapshot.workspaces_for_output(Some(output.id)).collect();
        workspaces.sort_by_key(|workspace| (workspace.index, workspace.id));
        if let Some(position) = workspaces
            .iter()
            .position(|workspace| workspace.id == output.active_workspace)
            && let Some(next) = position
                .checked_add_signed(direction as isize)
                .and_then(|next| workspaces.get(next))
            && let Some(control) = &self.control
        {
            control.activate_workspace(next.id);
        }
        self.dismiss_workspace_tooltip_for_bar(id)
    }

    pub(super) fn hub_workspaces(&self, id: window::Id, opacity: f32) -> Element<'_, cosmic::Action<Message>> {
        let selection = self.hub_workspace_selection(id);
        let width = selection.width();
        let output = self.output_for_bar(id);
        let active = output.map_or(0, |output| output.active_workspace);
        let palette = self.config.theme.palette();
        let foreground = color_with_opacity(self.config.theme.text_primary, opacity);
        let hidden_urgent = self.snapshot.windows.iter().any(|window| {
            window.urgent
                && !selection
                    .workspaces
                    .iter()
                    .any(|workspace| workspace.id == window.workspace)
                && self
                    .snapshot
                    .workspaces_for_output(output.map(|output| output.id))
                    .any(|workspace| workspace.id == window.workspace)
        });
        let mut dots = row::with_capacity(LIMIT).align_y(Alignment::Center);
        let mut label = selection
            .workspaces
            .iter()
            .find(|workspace| workspace.id == active)
            .map_or_else(
                || "Workspace overview".to_owned(),
                |workspace| {
                    format!(
                        "Workspace {} · {} · {} windows · Active · Open overview",
                        workspace.index, workspace.name, workspace.window_count
                    )
                },
            );
        for (index, workspace) in selection.workspaces.iter().enumerate() {
            let urgent = self
                .snapshot
                .windows
                .iter()
                .any(|window| window.workspace == workspace.id && window.urgent);
            if urgent {
                label.push_str(&format!(" · Workspace {} needs attention", workspace.index));
            }
            let transition = self.workspace_ui.transition(workspace.id, workspace.id == active);
            dots = dots.push(motion::geometry_driven(
                transition.revision(),
                move |now| {
                    let progress = transition.progress(now).clamp(0., 1.);
                    let color = if urgent {
                        palette.error
                    } else {
                        ferese_theme::mix(palette.muted, palette.accent, progress)
                    };
                    container(
                        container(cosmic::widget::Space::new())
                            .width(6. + 12. * progress)
                            .height(6.)
                            .class(theme::Container::custom(move |_| container::Style {
                                background: Some(color.scale_alpha(opacity).into()),
                                border: cosmic::iced::Border {
                                    radius: 3.0.into(),
                                    ..Default::default()
                                },
                                ..Default::default()
                            })),
                    )
                    // The first dot starts at the group's edge; only the gaps
                    // between dots need the two half-slot insets.
                    .padding(cosmic::iced::Padding {
                        left: if index == 0 { 0. } else { DOT_INSET },
                        right: DOT_INSET,
                        ..Default::default()
                    })
                    .center_y(28)
                    .into()
                },
                move |now| transition.active(now),
                |_| {},
                |_| None,
                None,
            ));
        }
        if selection.hidden > 0 {
            label.push_str(&format!(" · {} other workspaces", selection.hidden));
            if hidden_urgent {
                label.push_str(" · Another workspace needs attention");
            }
            dots = dots.push(
                container(
                    text(format!("+{}", selection.hidden))
                        .size(11)
                        .class(theme::Text::Color(if hidden_urgent {
                            palette.error.scale_alpha(opacity)
                        } else {
                            foreground
                        })),
                )
                .padding([0, 3]),
            );
        }
        let appearance = button::Style {
            text_color: Some(foreground),
            icon_color: Some(foreground),
            ..Default::default()
        };
        let control = button::custom(dots)
            .name(label)
            .padding(0)
            .width(width)
            .height(28)
            .class(theme::Button::Custom {
                active: Box::new(move |_, _| appearance),
                hovered: Box::new(move |_, _| appearance),
                pressed: Box::new(move |_, _| appearance),
                disabled: Box::new(move |_| appearance),
            })
            .on_press(cosmic::Action::App(Message::HubOverview(id)));
        super::scroll::area(control.into(), id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflow_keeps_the_active_empty_workspace_and_order() {
        let workspaces: Vec<_> = (1..=20)
            .map(|id| WorkspaceSnapshot {
                id,
                index: id as u32,
                name: id.to_string(),
                output: Some(1),
                window_count: u32::from(id != 20),
                visible: id == 20,
                focused: id == 20,
            })
            .collect();
        for active in [1, 8, 20] {
            let selection = select(workspaces.iter().collect(), active, 200.);
            assert!(selection.workspaces.iter().any(|workspace| workspace.id == active));
            assert!(
                selection
                    .workspaces
                    .windows(2)
                    .all(|pair| pair[0].index < pair[1].index)
            );
            assert_eq!(selection.workspaces.len(), 4);
            assert_eq!(selection.hidden, if active == 20 { 16 } else { 15 });
            let narrow = select(workspaces.iter().collect(), active, 148.);
            assert!(narrow.width() <= 148.);
            assert!(narrow.workspaces.iter().any(|workspace| workspace.id == active));
        }
    }

    #[test]
    fn trackpad_accumulates_and_resets_between_gestures() {
        let mut scroll = Scroll::default();
        let start = Instant::now();
        let pixels = |y| ScrollDelta::Pixels { x: 0., y };
        assert_eq!(scroll.step(pixels(-20.), start), None);
        assert_eq!(scroll.step(pixels(-20.), start + Duration::from_millis(10)), Some(1));
        assert_eq!(scroll.step(pixels(-100.), start + Duration::from_millis(20)), None);
        assert_eq!(scroll.step(pixels(20.), start + Duration::from_millis(200)), None);
        assert_eq!(scroll.step(pixels(20.), start + Duration::from_millis(500)), None);
        assert_eq!(scroll.step(pixels(20.), start + Duration::from_millis(510)), Some(-1));
        assert_eq!(scroll.step(pixels(f32::NAN), start + Duration::from_secs(1)), None);
    }
}
