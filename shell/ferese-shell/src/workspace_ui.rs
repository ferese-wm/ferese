use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{Element, Message, container, text};
use cosmic::app::Task;
use cosmic::iced::advanced::widget::{Id, Operation, operation};
use cosmic::iced::{Rectangle, window};
use cosmic::widget::icon;
use freedesktop_desktop_entry as fde;

use crate::{FereseShell, ShellSnapshot, motion};

impl FereseShell {
    pub(super) fn update_workspace_ui(&mut self) -> Task<Message> {
        let needs_icons = [
            &self.config.panels[0].start,
            &self.config.panels[0].center,
            &self.config.panels[0].end,
        ]
        .into_iter()
        .flat_map(|zone| &zone.groups)
        .flat_map(|group| &group.items)
        .any(|item| {
            item.visible
                && matches!(
                    item.kind,
                    crate::panel::ItemKind::Workspaces {
                        style: ferese_config::panel::WorkspaceStyle::AppIcons
                    }
                )
        });
        self.workspace_ui.update(&self.snapshot, self.config.animations);
        let icons = self.workspace_ui.load_icons(&self.snapshot, needs_icons);
        let tooltip = if self.workspace_ui.hovered.as_ref().is_some_and(|hover| {
            !self
                .snapshot
                .workspaces
                .iter()
                .any(|workspace| workspace.id == hover.workspace)
        }) {
            self.dismiss_workspace_tooltip()
        } else {
            Task::none()
        };
        Task::batch([icons, tooltip])
    }

    pub(super) fn workspace_icons_loaded(&mut self, result: Result<IconIndex, String>) -> Task<Message> {
        self.workspace_ui.icons_loading = false;
        match result {
            Ok(index) => {
                self.workspace_ui.entries = Some(index.entries);
                self.workspace_ui.icons.extend(index.icons);
                // Windows may have changed while discovery was in flight.
                self.update_workspace_ui()
            }
            Err(error) => {
                eprintln!("ferese-shell: could not load workspace icons: {error}");
                Task::none()
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct IconIndex {
    entries: Arc<Vec<fde::DesktopEntry>>,
    icons: HashMap<String, icon::Handle>,
}

fn discover_icons(entries: Option<Arc<Vec<fde::DesktopEntry>>>, app_ids: Vec<String>) -> IconIndex {
    let entries = entries.unwrap_or_else(|| {
        let mut seen = HashSet::new();
        Arc::new(
            fde::Iter::new(fde::default_paths())
                .entries::<&str>(None)
                .filter(|entry| seen.insert(entry.appid.clone()))
                .filter(|entry| !entry.hidden())
                .collect(),
        )
    });
    let icons = app_ids
        .into_iter()
        .map(|app_id| {
            let entry = fde::find_app_by_id(&entries, fde::unicase::Ascii::new(app_id.as_str()));
            let source = entry
                .and_then(fde::DesktopEntry::icon)
                .unwrap_or("application-x-executable");
            let handle = if source.starts_with('/') {
                icon::from_path(source.into())
            } else {
                icon::from_name(source)
                    .prefer_svg(true)
                    .size(32)
                    .fallback(Some(icon::IconFallback::Names(vec!["application-x-executable".into()])))
                    .handle()
            };
            (app_id, handle)
        })
        .collect();
    IconIndex { entries, icons }
}

#[derive(Default)]
pub(super) struct WorkspaceUi {
    entries: Option<Arc<Vec<fde::DesktopEntry>>>,
    icons: HashMap<String, icon::Handle>,
    icons_loading: bool,
    transitions: HashMap<u64, Transition>,
    hovered: Option<Hover>,
    pub(super) tooltip: Option<window::Id>,
    serial: u64,
}

#[derive(Clone)]
struct Hover {
    bar: window::Id,
    workspace: u64,
    target: Id,
    serial: u64,
}

struct FindBounds {
    target: Id,
    bar: window::Id,
    window: Option<window::Id>,
    found: Option<Rectangle>,
}

impl Operation<Option<Rectangle>> for FindBounds {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<Option<Rectangle>>)) {
        operate(self);
    }
    fn set_window_id(&mut self, id: window::Id) {
        self.window = Some(id);
    }
    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if id == Some(&self.target) && self.window == Some(self.bar) {
            self.found = Some(bounds);
        }
    }
    fn finish(&self) -> operation::Outcome<Option<Rectangle>> {
        operation::Outcome::Some(self.found)
    }
}

impl FereseShell {
    pub(super) fn dismiss_workspace_tooltip(&mut self) -> Task<Message> {
        self.workspace_ui.serial = self.workspace_ui.serial.wrapping_add(1);
        self.workspace_ui.hovered = None;
        self.workspace_ui.tooltip.take().map_or_else(Task::none, |id| {
            cosmic::task::message(cosmic::Action::Surface(cosmic::surface::action::destroy_popup(id)))
        })
    }

    pub(super) fn hover_workspace(
        &mut self,
        bar: window::Id,
        workspace: u64,
        target: Id,
        entered: bool,
    ) -> Task<Message> {
        if !entered {
            return if self
                .workspace_ui
                .hovered
                .as_ref()
                .is_some_and(|hover| hover.target == target)
            {
                self.dismiss_workspace_tooltip()
            } else {
                Task::none()
            };
        }
        let close = self.dismiss_workspace_tooltip();
        if self.menu.is_some() || self.system_modal.is_some() || self.overview_active {
            return close;
        }
        let serial = self.workspace_ui.serial;
        self.workspace_ui.hovered = Some(Hover {
            bar,
            workspace,
            target,
            serial,
        });
        close.chain(Task::perform(
            async { tokio::time::sleep(Duration::from_millis(300)).await },
            move |_| cosmic::Action::App(Message::WorkspaceTooltipDelay(serial)),
        ))
    }

    pub(super) fn locate_workspace_tooltip(&self, serial: u64) -> Task<Message> {
        let Some(hover) = self
            .workspace_ui
            .hovered
            .as_ref()
            .filter(|hover| hover.serial == serial)
        else {
            return Task::none();
        };
        cosmic::iced::advanced::widget::operate(FindBounds {
            target: hover.target.clone(),
            bar: hover.bar,
            window: None,
            found: None,
        })
        .map(move |bounds| cosmic::Action::App(Message::WorkspaceTooltipBounds(serial, bounds)))
    }

    pub(super) fn show_workspace_tooltip(&mut self, serial: u64, bounds: Option<Rectangle>) -> Task<Message> {
        use cosmic::iced::platform_specific::runtime::wayland::popup::{SctkPopupSettings, SctkPositioner};
        let Some(hover) = self
            .workspace_ui
            .hovered
            .as_ref()
            .filter(|hover| hover.serial == serial)
        else {
            return Task::none();
        };
        let Some(bounds) = bounds.filter(|bounds| bounds.width > 0. && bounds.height > 0.) else {
            return Task::none();
        };
        if self.workspace_ui.tooltip.is_some() {
            return Task::none();
        }
        if self.bar_hidden(hover.bar) || self.menu.is_some() || self.system_modal.is_some() || self.overview_active {
            return self.dismiss_workspace_tooltip();
        }
        if !self
            .snapshot
            .workspaces
            .iter()
            .any(|workspace| workspace.id == hover.workspace)
        {
            return self.dismiss_workspace_tooltip();
        }
        let id = window::Id::unique();
        let parent = hover.bar;
        let (anchor, gravity, gap) = match self.config.panels[0].edge {
            ferese_config::panel::Edge::Top => (2u32, 2u32, 6),
            ferese_config::panel::Edge::Bottom => (1u32, 1u32, -6),
        };
        self.workspace_ui.tooltip = Some(id);
        let action = cosmic::surface::action::app_popup::<Self>(
            |_| Default::default(),
            move |_| SctkPopupSettings {
                id,
                parent,
                parent_size: None,
                grab: false,
                close_with_children: false,
                input_zone: Some(Vec::new()),
                positioner: SctkPositioner {
                    anchor_rect: Rectangle {
                        x: bounds.x.round() as i32,
                        y: bounds.y.round() as i32,
                        width: bounds.width.round() as i32,
                        height: bounds.height.round() as i32,
                    },
                    anchor: anchor.try_into().unwrap(),
                    gravity: gravity.try_into().unwrap(),
                    offset: (0, gap),
                    constraint_adjustment: 3,
                    size_limits: cosmic::iced::Limits::NONE.max_width(320.).max_height(64.),
                    ..Default::default()
                },
            },
            Some(Box::new(Self::view_workspace_tooltip)),
        );
        cosmic::task::message(cosmic::Action::Surface(action))
    }

    fn view_workspace_tooltip(&self) -> Element<'_, cosmic::Action<Message>> {
        let Some(workspace) = self.workspace_ui.hovered.as_ref().and_then(|hover| {
            self.snapshot
                .workspaces
                .iter()
                .find(|workspace| workspace.id == hover.workspace)
        }) else {
            return text("").into();
        };
        let active = self
            .snapshot
            .outputs
            .iter()
            .any(|output| output.active_workspace == workspace.id);
        let label = ferese_theme::workspaces::tooltip_label(&ferese_theme::workspaces::Workspace {
            name: (&workspace.name).into(),
            index: workspace.index,
            window_count: workspace.window_count,
            active,
            visible_elsewhere: false,
        });
        cosmic::widget::autosize::autosize(
            container(
                text(label)
                    .size(11)
                    .wrapping(cosmic::iced::widget::text::Wrapping::None)
                    .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                        cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
                    ))
                    .class(cosmic::theme::Text::Color(crate::color(self.config.theme.text_primary))),
            )
            .padding([8, 10])
            .class(ferese_theme::controls::surface(
                crate::color(self.config.theme.surface_popover),
                self.config.theme.material_radius.min(10.),
            )),
            Id::new("ferese-workspace-tooltip"),
        )
        .limits(cosmic::iced::Limits::NONE.max_width(320.).max_height(64.))
        .into()
    }
}

#[derive(Clone, Copy)]
pub(super) struct Transition {
    from: f32,
    to: f32,
    started: Instant,
    duration: Duration,
}

impl Transition {
    pub(super) fn progress(self, now: Instant) -> f32 {
        if self.duration.is_zero() {
            return self.to;
        }

        let t = (now.saturating_duration_since(self.started).as_secs_f32() / self.duration.as_secs_f32()).min(1.);
        self.from + (self.to - self.from) * t * t * (3. - 2. * t)
    }

    pub(super) fn active(self, now: Instant) -> bool {
        self.from != self.to && now.saturating_duration_since(self.started) < self.duration
    }

    pub(super) fn revision(self) -> Option<Instant> {
        Some(self.started)
    }
}

impl WorkspaceUi {
    pub(super) fn update(&mut self, snapshot: &ShellSnapshot, settings: motion::Settings) {
        self.update_at(snapshot, settings, Instant::now());
    }

    fn update_at(&mut self, snapshot: &ShellSnapshot, settings: motion::Settings, now: Instant) {
        let duration = if settings.motion_enabled() {
            Duration::from_secs_f64(0.16 / settings.speed)
        } else {
            Duration::ZERO
        };

        self.transitions
            .retain(|id, _| snapshot.workspaces.iter().any(|workspace| workspace.id == *id));

        for workspace in &snapshot.workspaces {
            let active = snapshot
                .outputs
                .iter()
                .any(|output| Some(output.id) == workspace.output && output.active_workspace == workspace.id);
            let target = if active { 1. } else { 0. };
            let transition = self.transitions.entry(workspace.id).or_insert(Transition {
                from: target,
                to: target,
                started: now,
                duration,
            });

            if transition.to != target || transition.duration != duration {
                *transition = Transition {
                    from: transition.progress(now),
                    to: target,
                    started: now,
                    duration,
                };
            }
        }
    }

    fn load_icons(&mut self, snapshot: &ShellSnapshot, needed: bool) -> Task<Message> {
        if !needed || self.icons_loading {
            return Task::none();
        }
        let app_ids: Vec<_> = snapshot
            .windows
            .iter()
            .map(|window| &window.app_id)
            .filter(|id| !id.is_empty() && !self.icons.contains_key(*id))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if app_ids.is_empty() {
            return Task::none();
        }
        self.icons_loading = true;
        let entries = self.entries.clone();
        Task::perform(
            async move {
                tokio::task::spawn_blocking(move || discover_icons(entries, app_ids))
                    .await
                    .map_err(|error| error.to_string())
            },
            |result| cosmic::Action::App(Message::WorkspaceIconsLoaded(result)),
        )
    }

    pub(super) fn transition(&self, workspace: u64, active: bool) -> Transition {
        self.transitions.get(&workspace).copied().unwrap_or(Transition {
            from: if active { 1. } else { 0. },
            to: if active { 1. } else { 0. },
            started: Instant::now(),
            duration: Duration::ZERO,
        })
    }

    pub(super) fn apps(&self, workspace: u64, snapshot: &ShellSnapshot) -> Vec<icon::Handle> {
        snapshot
            .windows
            .iter()
            .filter(|window| window.workspace == workspace && !window.app_id.is_empty())
            .map(|window| &window.app_id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .take(2)
            .map(|id| {
                self.icons
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| icon::from_svg_bytes(ferese_theme::icons::APPLICATION))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{OutputSnapshot, WorkspaceSnapshot};

    fn snapshot(active: u64) -> ShellSnapshot {
        ShellSnapshot {
            outputs: vec![OutputSnapshot {
                id: 4,
                name: "test".into(),
                active_workspace: active,
                focused: true,
            }],
            workspaces: [1, 2]
                .into_iter()
                .map(|id| WorkspaceSnapshot {
                    id,
                    name: id.to_string(),
                    output: Some(4),
                    index: id as u32,
                    window_count: 0,
                    visible: id == active,
                    focused: id == active,
                })
                .collect(),
            windows: Vec::new(),
        }
    }

    fn add_window(snapshot: &mut ShellSnapshot, app_id: &str) {
        snapshot.windows.push(crate::control::WindowSnapshot {
            id: snapshot.windows.len() as u64,
            workspace: 1,
            app_id: app_id.into(),
            title: String::new(),
            focused: false,
            urgent: false,
            fullscreen: false,
            floating: false,
        });
    }

    #[test]
    fn discovery_is_deferred_and_only_one_request_runs_at_a_time() {
        let mut ui = WorkspaceUi::default();
        let mut snapshot = snapshot(1);
        add_window(&mut snapshot, "new-app");
        ui.update(&snapshot, motion::Settings::default());
        assert!(ui.entries.is_none());
        assert_eq!(ui.load_icons(&snapshot, false).units(), 0);
        assert!(!ui.icons_loading);
        assert_eq!(ui.load_icons(&snapshot, true).units(), 1);
        assert!(ui.icons_loading);
        assert!(ui.entries.is_none(), "scheduling must not perform filesystem discovery");
        assert_eq!(ui.apps(1, &snapshot).len(), 1, "new apps have a bundled placeholder");
        assert_eq!(ui.load_icons(&snapshot, true).units(), 0);
        assert!(ui.entries.is_none());
        assert!(ui.icons.is_empty());
    }

    #[test]
    fn icon_publication_preserves_existing_icons_and_queues_new_windows() {
        let mut shell = crate::tests::shell_with_measured_panel();
        for group in &mut shell.config.panels[0].start.groups {
            for item in &mut group.items {
                if matches!(item.kind, crate::panel::ItemKind::Workspaces { .. }) {
                    item.kind = crate::panel::ItemKind::Workspaces {
                        style: ferese_config::panel::WorkspaceStyle::AppIcons,
                    };
                }
            }
        }
        shell.snapshot = snapshot(1);
        add_window(&mut shell.snapshot, "existing");
        add_window(&mut shell.snapshot, "discovered");
        shell
            .workspace_ui
            .icons
            .insert("existing".into(), icon::from_svg_bytes(ferese_theme::icons::SETTINGS));
        use std::hash::{Hash, Hasher};
        let identity = |handle: &icon::Handle| {
            let mut hash = std::collections::hash_map::DefaultHasher::new();
            handle.hash(&mut hash);
            hash.finish()
        };
        let existing = identity(&shell.workspace_ui.icons["existing"]);
        drop(shell.update_workspace_ui());
        assert!(shell.workspace_ui.icons_loading);
        assert_eq!(identity(&shell.workspace_ui.icons["existing"]), existing);
        add_window(&mut shell.snapshot, "arrived-during-discovery");
        drop(shell.workspace_icons_loaded(Ok(IconIndex {
            entries: Arc::new(Vec::new()),
            icons: HashMap::from([("discovered".into(), icon::from_svg_bytes(ferese_theme::icons::OVERVIEW))]),
        })));
        assert_eq!(identity(&shell.workspace_ui.icons["existing"]), existing);
        assert!(shell.workspace_ui.icons.contains_key("discovered"));
        assert!(
            shell.workspace_ui.icons_loading,
            "newly arrived windows need a follow-up request"
        );
        drop(shell.workspace_icons_loaded(Err("discovery failed".into())));
        assert!(!shell.workspace_ui.icons_loading);
        assert_eq!(identity(&shell.workspace_ui.icons["existing"]), existing);
    }

    #[test]
    fn switching_during_a_transition_preserves_position_and_reduced_motion_snaps() {
        let mut ui = WorkspaceUi::default();
        let now = Instant::now();
        let settings = motion::Settings::default();
        ui.update_at(&snapshot(1), settings, now);
        assert_eq!(ui.transition(1, true).progress(now), 1.);
        ui.update_at(&snapshot(2), settings, now);
        let midpoint = now + Duration::from_millis(80);
        assert_eq!(ui.transition(1, false).progress(midpoint), 0.5);
        ui.update_at(&snapshot(1), settings, midpoint);
        assert_eq!(ui.transition(1, true).progress(midpoint), 0.5);
        assert_eq!(ui.transition(2, false).progress(midpoint), 0.5);
        assert_eq!(
            ui.transition(1, true).progress(midpoint + Duration::from_millis(160)),
            1.
        );
        ui.update_at(
            &snapshot(2),
            motion::Settings {
                reduced_motion: true,
                ..settings
            },
            midpoint,
        );
        assert_eq!(ui.transition(1, false).progress(midpoint), 0.);
        assert_eq!(ui.transition(2, true).progress(midpoint), 1.);
        assert!(!ui.transition(2, true).active(midpoint));
        assert!(ui.entries.is_none(), "other styles must not scan desktop applications");
        ui.update_at(&ShellSnapshot::default(), settings, midpoint);
        assert!(ui.transitions.is_empty());
    }

    #[test]
    fn active_state_belongs_to_the_workspace_output() {
        let mut ui = WorkspaceUi::default();
        let mut snapshot = snapshot(1);
        snapshot.workspaces[0].output = Some(5);
        ui.update(&snapshot, motion::Settings::default());
        assert_eq!(ui.transition(1, false).progress(Instant::now()), 0.);
    }

    #[test]
    fn app_slots_are_distinct_and_do_not_jump_when_windows_reorder() {
        use crate::control::WindowSnapshot;
        use std::hash::{Hash, Hasher};
        let mut ui = WorkspaceUi::default();
        for (id, svg) in [
            ("a", ferese_theme::icons::SETTINGS),
            ("b", ferese_theme::icons::FERESE),
            ("c", ferese_theme::icons::OVERVIEW),
        ] {
            ui.icons.insert(id.into(), icon::from_svg_bytes(svg));
        }
        let mut snapshot = snapshot(1);
        snapshot.windows = [(1, "b"), (1, "a"), (1, "b"), (1, "c"), (2, "a"), (1, "")]
            .into_iter()
            .enumerate()
            .map(|(id, (workspace, app_id))| WindowSnapshot {
                id: id as u64,
                workspace,
                app_id: app_id.into(),
                title: String::new(),
                focused: false,
                urgent: false,
                fullscreen: false,
                floating: false,
            })
            .collect();
        let fingerprint = |handles: Vec<icon::Handle>| {
            handles
                .into_iter()
                .map(|handle| {
                    let mut hash = std::collections::hash_map::DefaultHasher::new();
                    handle.hash(&mut hash);
                    hash.finish()
                })
                .collect::<Vec<_>>()
        };
        let original = fingerprint(ui.apps(1, &snapshot));
        assert_eq!(original.len(), 2);
        assert_ne!(original[0], original[1]);
        snapshot.windows.reverse();
        snapshot.windows[0].focused = true;
        assert_eq!(fingerprint(ui.apps(1, &snapshot)), original);
        assert_eq!(ui.apps(2, &snapshot).len(), 1);
        assert!(ui.apps(3, &snapshot).is_empty());
    }

    #[test]
    fn bounds_lookup_ignores_matching_widgets_on_other_outputs() {
        let target = Id::new("workspace:test");
        let bar = window::Id::unique();
        let mut operation = FindBounds {
            target: target.clone(),
            bar,
            window: None,
            found: None,
        };
        let bounds = Rectangle::with_size(cosmic::iced::Size::new(24., 28.));
        operation.set_window_id(window::Id::unique());
        operation.container(Some(&target), bounds);
        assert_eq!(operation.found, None);
        operation.set_window_id(bar);
        operation.container(Some(&target), bounds);
        assert_eq!(operation.found, Some(bounds));
    }

    #[test]
    fn stale_hover_messages_cannot_replace_or_reopen_tooltips() {
        let mut shell = crate::tests::shell_with_measured_panel();
        shell.snapshot = snapshot(1);
        let bar = shell.outputs[0].bar;
        let first = Id::new("workspace:first");
        let second = Id::new("workspace:second");
        let _ = shell.hover_workspace(bar, 1, first.clone(), true);
        let old_serial = shell.workspace_ui.serial;
        let _ = shell.hover_workspace(bar, 2, second.clone(), true);
        let _ = shell.hover_workspace(bar, 1, first, false);
        assert_eq!(shell.workspace_ui.hovered.as_ref().unwrap().target, second);
        let bounds = Some(Rectangle::with_size(cosmic::iced::Size::new(24., 28.)));
        let _ = shell.show_workspace_tooltip(old_serial, bounds);
        assert!(shell.workspace_ui.tooltip.is_none());
        let current = shell.workspace_ui.serial;
        let _ = shell.show_workspace_tooltip(current, bounds);
        assert!(shell.workspace_ui.tooltip.is_some());
        let _ = shell.dismiss_workspace_tooltip();
        let _ = shell.show_workspace_tooltip(current, bounds);
        assert!(shell.workspace_ui.tooltip.is_none());
        let _ = shell.hover_workspace(bar, 2, second, true);
        let current = shell.workspace_ui.serial;
        let _ = shell.show_workspace_tooltip(current, bounds);
        shell.snapshot.workspaces.retain(|workspace| workspace.id != 2);
        let _ = shell.update_workspace_ui();
        assert!(shell.workspace_ui.tooltip.is_none());
        assert!(shell.workspace_ui.hovered.is_none());
    }
}
