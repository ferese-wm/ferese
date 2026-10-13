//! Shared Activities item and Morph presentation. Publishers provide data only.
mod compact;
mod ring;
pub(crate) mod subscription;
use crate::{FereseShell, Message, status_ui};
use cosmic::app::Task;
use cosmic::iced::{Alignment, Length, Subscription, window};
use cosmic::widget::{button, column, container, row};
use cosmic::{Element, theme};
use ferese_activities::{Activity, Id, Selection, State, Template, Value, ranked, reconcile_focus};
use ferese_animation::{AnimatedValue, SpringConfig};
use ferese_ipc::activities::{Snapshot, now_ms};
use ferese_theme::icons;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

// Expanded lists retain result rows under DND. Compact selection applies DND
// separately, so rendered rows and keyboard focus always share this projection.
fn list_entries<'a>(snapshot: &'a Snapshot, native: &'a [Activity], now: u64, output: &str) -> Vec<&'a Activity> {
    ranked(
        snapshot.activities.iter().chain(native.iter()),
        now,
        snapshot.limits,
        output,
        false,
    )
}

pub(crate) struct Model {
    pub snapshot: Arc<Snapshot>,
    native: Vec<Activity>,
    flat: std::collections::HashMap<window::Id, Presentation>,
}
impl Default for Model {
    fn default() -> Self {
        Self {
            snapshot: Arc::new(Snapshot::default()),
            native: vec![],
            flat: Default::default(),
        }
    }
}
#[derive(Default)]
pub(crate) struct Presentation {
    compact: compact::Compact,
    pub selection: Selection,
    pub focus: Option<Id>,
    rows: Vec<Id>,
    divider: Option<AnimatedValue>,
    tints: BTreeMap<Id, AnimatedValue>,
}
impl Presentation {
    pub fn advance(&mut self, elapsed: Duration, reduced: bool) {
        let spring = SpringConfig {
            stiffness: 700.,
            damping: 45.,
            position_tolerance: 0.002,
            velocity_tolerance: 0.002,
            ..Default::default()
        };
        for value in self.divider.iter_mut().chain(self.tints.values_mut()) {
            if reduced {
                value.snap();
            } else {
                value.advance(elapsed, spring);
            }
        }
    }
    pub fn active(&self) -> bool {
        self.divider
            .iter()
            .chain(self.tints.values())
            .any(AnimatedValue::is_animating)
    }
}
fn activity_widget_id(bar: window::Id, id: &Id) -> cosmic::iced::advanced::widget::Id {
    cosmic::iced::advanced::widget::Id::new(format!(
        "activity-{bar:?}-{}-{}-{}-{}",
        id.session, id.generation, id.owner.pid, id.owner.birth
    ))
}
struct FocusedActivity {
    bar: window::Id,
    rows: Vec<Id>,
    focused: Option<Id>,
}
impl cosmic::iced::advanced::widget::Operation<Option<Id>> for FocusedActivity {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn cosmic::iced::advanced::widget::Operation<Option<Id>>)) {
        operate(self);
    }
    fn focusable(
        &mut self,
        id: Option<&cosmic::iced::advanced::widget::Id>,
        _: cosmic::iced::Rectangle,
        state: &mut dyn cosmic::iced::advanced::widget::operation::Focusable,
    ) {
        if state.is_focused()
            && let Some(row) = self
                .rows
                .iter()
                .find(|row| Some(&activity_widget_id(self.bar, row)) == id)
        {
            self.focused = Some(row.clone());
        }
    }
    fn finish(&self) -> cosmic::iced::advanced::widget::operation::Outcome<Option<Id>> {
        cosmic::iced::advanced::widget::operation::Outcome::Some(self.focused.clone())
    }
}

fn icon(name: &str) -> &'static [u8] {
    match name {
        "timer" => icons::ACTIVITY_TIMER,
        "media" => icons::MEDIA,
        "microphone" => icons::MIC,
        "camera" | "capture" => icons::RECORD,
        "notification" => icons::NOTIFICATIONS,
        "hammer" => icons::ACTIVITY_HAMMER,
        "download" => icons::ACTIVITY_DOWNLOAD,
        _ => icons::FERESE,
    }
}
fn progress_percent(activity: &Activity) -> Option<u8> {
    match activity.data.value {
        Value::Progress(value) if activity.state == State::Running => Some((value.clamp(0., 1.) * 100.).round() as u8),
        _ => None,
    }
}

fn detail(activity: &Activity) -> Option<String> {
    progress_percent(activity).is_none().then(|| label(activity))
}

fn label(activity: &Activity) -> String {
    if activity.state != State::Running {
        return activity.result_message.clone().unwrap_or_else(|| {
            if activity.state == State::Done {
                "Finished".into()
            } else {
                "Failed".into()
            }
        });
    }
    match &activity.data.value {
        Value::Text(text) => text.clone(),
        Value::Progress(progress) => format!("{}%", (progress * 100.).round() as u32),
        Value::Countdown { .. } => {
            let seconds = activity.data.value.remaining_ms(now_ms()).unwrap_or(0).div_ceil(1_000);
            format!("{}:{:02}", seconds / 60, seconds % 60)
        }
        Value::Indeterminate => {
            if activity.state == State::Running {
                "In progress".into()
            } else if activity.state == State::Done {
                "Finished".into()
            } else {
                "Failed".into()
            }
        }
    }
}
fn native_text(text: &str) -> String {
    let mut bounded = String::new();
    for character in text.chars().filter(|character| !character.is_control()) {
        if bounded.len() + character.len_utf8() > ferese_activities::MAX_TEXT {
            break;
        }
        bounded.push(character);
    }
    bounded
}

impl FereseShell {
    fn refresh_native_activities(&mut self) {
        use ferese_activities::{Data, Owner, Source};
        let now = now_ms();
        let focused = self
            .snapshot
            .outputs
            .iter()
            .find(|output| output.focused)
            .or_else(|| self.snapshot.outputs.first())
            .map(|output| output.name.clone());
        let owner = Owner {
            pid: std::process::id(),
            birth: 0,
        };
        let mut observed = Vec::new();
        if let Some(player) = &self.media.snapshot.selected
            && matches!(
                player.status,
                ferese_ipc::media::Playback::Playing | ferese_ipc::media::Playback::Paused
            )
        {
            observed.push((
                format!("media:{}", player.owner),
                0,
                Data {
                    source: Source::Media,
                    output: focused.clone(),
                    priority: 60,
                    icon: "media".into(),
                    title: native_text(player.label()),
                    value: Value::Indeterminate,
                    template: Template::Media {
                        artist: native_text(&player.artist),
                        playing: player.status == ferese_ipc::media::Playback::Playing,
                    },
                },
            ));
        }
        if self.config.activities.sources.notification_bridge
            && self.config.notifications.show_popups
            && !self.notifications.dnd
        {
            for notice in self
                .notifications
                .entries
                .iter()
                .filter(|notice| notice.live && notice.unread)
                .take(16)
            {
                observed.push((
                    format!("notification:{}", notice.id),
                    u64::from(notice.id),
                    Data {
                        source: Source::Notification,
                        output: focused.clone(),
                        priority: 0,
                        icon: "notification".into(),
                        title: native_text(&notice.title),
                        value: Value::Text(native_text(&notice.app)),
                        template: Template::Progress,
                    },
                ));
            }
        }
        let mut previous = std::mem::take(&mut self.activities.native);
        for (local, generation, mut data) in observed {
            let id = Id {
                owner,
                local,
                generation,
                session: 0,
            };
            if let Some(index) = previous.iter().position(|entry| entry.id == id) {
                let mut entry = previous.remove(index);
                // Keep source identities anchored to their creation output.
                if self
                    .snapshot
                    .outputs
                    .iter()
                    .any(|output| Some(&output.name) == entry.data.output.as_ref())
                {
                    data.output = entry.data.output.clone();
                }
                if data.validate(now).is_ok() {
                    entry.data = data;
                    self.activities.native.push(entry);
                }
            } else if data.validate(now).is_ok() {
                self.activities.native.push(Activity {
                    id,
                    data,
                    state: State::Running,
                    created_at: now,
                    eligible_at: now,
                    refreshed_at: now,
                    lease_deadline: None,
                    result_deadline: None,
                    result_message: None,
                    flash_on_fail: false,
                    result_flash_ms: 0,
                    group: None,
                });
            }
        }
    }
    pub fn sync_activity_presentations(&mut self) {
        self.activities
            .flat
            .retain(|bar, _| self.outputs.iter().any(|output| output.bar == *bar));
        self.refresh_native_activities();
        let now = now_ms();
        let protected = self.protected_activity_slots();
        let dnd = self.notifications.dnd;
        for output in &self.outputs {
            if output.runtime.hub.is_none() {
                let name = self
                    .output_for_bar(output.bar)
                    .map(|output| output.name.as_str())
                    .unwrap_or("")
                    .to_owned();
                let entries = list_entries(&self.activities.snapshot, &self.activities.native, now, &name);
                let compact: Vec<_> = entries
                    .iter()
                    .copied()
                    .filter(|entry| entry.eligible(now, dnd) && entry.data.source != ferese_activities::Source::Media)
                    .collect();
                let presentation = self.activities.flat.entry(output.bar).or_default();
                presentation
                    .selection
                    .select(&compact, self.activities.snapshot.limits, dnd, 0);
                let rows = entries.iter().map(|entry| entry.id.clone()).collect::<Vec<_>>();
                reconcile_focus(&mut presentation.focus, &presentation.rows, &rows);
                presentation.rows = rows;
                continue;
            }
            let runtime = output.runtime.hub.as_ref().unwrap();
            let name = self
                .output_for_bar(output.bar)
                .map(|output| output.name.as_str())
                .unwrap_or("");
            let entries = list_entries(&self.activities.snapshot, &self.activities.native, now, name);
            let workspace_width = self.hub_workspace_width(output.bar) as f32;
            let panel = self.panel_for_bar(output.bar);
            let available = super::panel_hub::content_width(panel, output.size) as f32
                - if workspace_width > 0. {
                    workspace_width + 10.
                } else {
                    0.
                };
            let mut runtime = runtime.borrow_mut();
            let before = runtime.activities.selection.visible.clone();
            let presentation = &mut runtime.activities;
            let compact: Vec<_> = entries
                .iter()
                .copied()
                .filter(|entry| entry.eligible(now, dnd) && entry.data.source != ferese_activities::Source::Media)
                .collect();
            let compact = if self.compact_activities_configured(output.bar) {
                compact
            } else {
                Vec::new()
            };
            presentation
                .selection
                .select(&compact, self.activities.snapshot.limits, dnd, protected);
            let rows: Vec<_> = entries.iter().map(|entry| entry.id.clone()).collect();
            reconcile_focus(&mut presentation.focus, &presentation.rows, &rows);
            presentation.rows = rows;
            presentation
                .tints
                .retain(|id, _| presentation.selection.visible.contains(id));
            for entry in entries
                .iter()
                .filter(|entry| presentation.selection.visible.contains(&entry.id))
            {
                if entry.state != State::Running {
                    let age = now.saturating_sub(
                        entry
                            .result_deadline
                            .unwrap_or(now)
                            .saturating_sub(entry.result_flash_ms),
                    );
                    presentation.tints.entry(entry.id.clone()).or_insert_with(|| {
                        let mut value = AnimatedValue::new(if age < 400 { 1. } else { 0. });
                        value.set_target(0.);
                        value
                    });
                }
            }
            let target = f64::from(presentation.selection.visible.len() == 2);
            presentation
                .divider
                .get_or_insert_with(|| AnimatedValue::new(0.))
                .retarget_preserving_motion(target);
            presentation
                .compact
                .update(&entries, &presentation.selection, self.render.font, available);
            let compact_width = presentation.compact.width;
            if !presentation.selection.visible.is_empty() {
                runtime.idle_content_width = Some(f64::from(compact_width));
            } else if !before.is_empty() {
                // Ordinary idle layout owns this measurement. Invalidate it only
                // when leaving activity content, not on every background refresh.
                runtime.idle_content_width = None;
            }
            // Compact ranking changes must not replay the expanded panel's
            // crossfade: its live rows update in place, preserving focus/scroll.
            if self.snapshot.binding_mode.is_none() {
                if before != runtime.activities.selection.visible
                    && matches!(
                        runtime.hub.mode(),
                        ferese_panel::morph::Mode::Idle | ferese_panel::morph::Mode::Peek
                    )
                {
                    runtime.hub.replace_content();
                }
                runtime.refresh_activity_size();
            }
        }
    }
    pub fn activity_tick_subscription(&self) -> Subscription<()> {
        let visible = self.activities.snapshot.activities.iter().any(|activity| {
            if !matches!(activity.data.value, Value::Countdown { .. })
                || activity.state != State::Running
                || activity.data.value.remaining_ms(now_ms()) == Some(0)
                || !activity.eligible(now_ms(), self.notifications.dnd)
            {
                return false;
            }
            self.outputs.iter().filter(|output| !output.hidden).any(|output| {
                let list = self
                    .menu
                    .as_ref()
                    .is_some_and(|menu| menu.kind == status_ui::Menu::Activities && menu.anchor.parent == output.bar);
                let routed = self
                    .output_for_bar(output.bar)
                    .is_some_and(|output| activity.data.output.as_deref() == Some(&output.name));
                if let Some(runtime) = &output.runtime.hub {
                    let runtime = runtime.borrow();
                    routed
                        && (runtime.activities.selection.visible.contains(&activity.id)
                            || runtime.hub.mode()
                                == ferese_panel::morph::Mode::Expanded(ferese_panel::morph::Content::Activities))
                } else {
                    routed
                        && (list
                            || self.activities.flat.get(&output.bar).is_some_and(|presentation| {
                                self.compact_activities_configured(output.bar)
                                    && presentation.selection.visible.contains(&activity.id)
                            }))
                }
            })
        });
        if visible {
            cosmic::iced::time::every(Duration::from_secs(1)).map(|_| ())
        } else {
            Subscription::none()
        }
    }
    fn activity_icon(
        &self,
        activity: &Activity,
        color: cosmic::iced::Color,
        animate: bool,
    ) -> Element<'_, cosmic::Action<Message>> {
        let reduced = !self.render.motion.motion_enabled();
        let progress = match activity.data.value {
            Value::Progress(progress) => Some(progress),
            _ => None,
        };
        let ring_color = if activity.state == State::Running {
            self.config.theme.palette().accent.scale_alpha(color.a)
        } else {
            color
        };
        let ring = ring::view(
            progress,
            animate && activity.state == State::Running,
            reduced,
            ring_color,
        );
        let center: Element<'_, cosmic::Action<Message>> = if let Some(percent) = progress_percent(activity) {
            self.render
                .text(percent.to_string())
                .size(if percent == 100 { 9. } else { 10. })
                .line_height(cosmic::iced::widget::text::LineHeight::Relative(1.))
                .class(theme::Text::Color(color))
                .into()
        } else {
            icons::tinted(icon(&activity.data.icon), 14, color).into()
        };
        cosmic::iced::widget::stack![ring, container(center).center_x(26).center_y(26)].into()
    }
    pub fn activity_available(&self, id: window::Id) -> bool {
        let name = self.output_for_bar(id).map(|output| output.name.as_str()).unwrap_or("");
        ranked(
            self.activities
                .snapshot
                .activities
                .iter()
                .chain(self.activities.native.iter()),
            now_ms(),
            self.activities.snapshot.limits,
            name,
            self.notifications.dnd,
        )
        .iter()
        .any(|entry| entry.data.source != ferese_activities::Source::Media)
    }
    pub fn activities_configured(&self, id: window::Id) -> bool {
        self.has_activities_item(id, false)
    }

    pub fn compact_activities_configured(&self, id: window::Id) -> bool {
        self.has_activities_item(id, true)
    }

    fn has_activities_item(&self, id: window::Id, compact: bool) -> bool {
        let panel = self.panel_for_bar(id);
        [&panel.start, &panel.center, &panel.end]
            .into_iter()
            .flat_map(|zone| &zone.groups)
            .flat_map(|group| &group.items)
            .any(|item| {
                item.visible
                    && item.kind == ferese_panel::ItemKind::Activities
                    && (!compact || item.overflow != ferese_panel::OverflowPolicy::Always)
            })
    }
    pub fn activity_hover(&mut self, id: window::Id, entered: bool) {
        if let Some(presentation) = self.activities.flat.get_mut(&id) {
            if entered {
                presentation.selection.enter();
            } else {
                presentation.selection.leave();
            }
        }
    }
    pub fn view_activity_item(
        &self,
        id: window::Id,
        item: &ferese_panel::Item,
        representation: ferese_panel::Representation,
        selected: bool,
        width: Option<f32>,
        height: f32,
    ) -> (Element<'_, cosmic::Action<Message>>, bool) {
        use ferese_panel::Representation;
        let Some(presentation) = self.activities.flat.get(&id) else {
            return (container(self.render.text("")).width(0).into(), false);
        };
        let Some(activity) = presentation.selection.visible.first().and_then(|id| {
            self.activities
                .snapshot
                .activities
                .iter()
                .chain(self.activities.native.iter())
                .find(|entry| &entry.id == id)
        }) else {
            return (container(self.render.text("")).width(0).into(), false);
        };
        let palette = self.config.theme.palette();
        let result_age = activity
            .result_deadline
            .map(|deadline| now_ms().saturating_sub(deadline.saturating_sub(activity.result_flash_ms)));
        let foreground = if result_age.is_some_and(|age| age < 400) && activity.state == State::Failed {
            palette.error
        } else if result_age.is_some_and(|age| age < 400) && activity.state == State::Done {
            palette.accent
        } else {
            palette.text
        };
        let mut content = row![self.activity_icon(activity, foreground, width.is_some())]
            .spacing(6)
            .align_y(Alignment::Center);
        let budget = if representation == Representation::Wide {
            190.
        } else {
            112.
        };
        if representation != Representation::Icon {
            content = content.push(
                self.render
                    .text(if representation == Representation::Wide {
                        detail(activity).map_or_else(
                            || activity.data.title.clone(),
                            |detail| format!("{} · {detail}", activity.data.title),
                        )
                    } else {
                        activity.data.title.clone()
                    })
                    .size(13)
                    .width(Length::Fill)
                    .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                        cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
                    )),
            );
        }
        let hidden = presentation.selection.hidden + presentation.selection.visible.len().saturating_sub(1);
        if hidden > 0 && !self.notifications.dnd && self.activities.snapshot.limits.show_badge {
            content = content.push(self.render.text(format!("+{hidden}")).size(11));
        }
        let panel = self.panel_for_bar(id).id.clone();
        let item = item.id.clone();
        let button = button::custom(crate::bar_content(content, height))
            .name(format!(
                "Activities: {}, {}. Open list",
                activity.data.title,
                label(activity)
            ))
            .selected(selected)
            .height(height)
            .padding([0, 5])
            .width(width.map_or(
                if representation == Representation::Icon {
                    Length::Shrink
                } else {
                    Length::Fixed(budget)
                },
                Length::Fixed,
            ))
            .on_press_with_rectangle(move |offset, bounds| {
                cosmic::Action::App(Message::OpenPopover(
                    status_ui::Menu::Activities,
                    status_ui::PopoverAnchor {
                        parent: id,
                        panel: panel.clone(),
                        item: Some(item.clone()),
                        rectangle: cosmic::iced::Rectangle {
                            x: (bounds.x - offset.x).round() as i32,
                            y: (bounds.y - offset.y).round() as i32,
                            width: bounds.width.round() as i32,
                            height: bounds.height.round() as i32,
                        },
                    },
                ))
            });
        (
            cosmic::iced::widget::mouse_area(crate::motion::button(self.render, button, palette.text, selected, 1.))
                .on_enter(cosmic::Action::App(Message::ActivityHover(id, true)))
                .on_exit(cosmic::Action::App(Message::ActivityHover(id, false)))
                .into(),
            true,
        )
    }
    pub fn has_compact_activities(&self, id: window::Id) -> bool {
        if !self.compact_activities_configured(id) {
            return false;
        }
        self.outputs
            .iter()
            .find(|entry| entry.bar == id)
            .and_then(|entry| entry.runtime.hub.as_ref())
            .is_some_and(|runtime| !runtime.borrow().activities.selection.visible.is_empty())
    }
    pub(crate) fn activity_window(&self, activity: &Id) -> Option<u64> {
        if !self
            .activities
            .snapshot
            .activities
            .iter()
            .any(|entry| &entry.id == activity)
        {
            return None;
        }
        let target = self
            .activities
            .snapshot
            .windows
            .iter()
            .find(|target| &target.activity == activity)?;
        self.snapshot
            .windows
            .iter()
            .any(|window| window.id == target.window)
            .then_some(target.window)
    }

    pub fn activate_activity(&mut self, id: window::Id, activity: Id) -> Task<Message> {
        self.focus_activity(id, activity.clone());
        let Some(target) = self.activity_window(&activity) else {
            return Task::none();
        };
        let Some(control) = &self.control else {
            return Task::none();
        };
        control.activate_window(target);
        if let Some(bar) = self.activity_list_bar(id)
            && self
                .outputs
                .iter()
                .any(|output| output.bar == bar && output.runtime.hub.is_some())
        {
            return self.close_hub(bar);
        }
        self.close_menu()
    }

    pub fn focus_activity(&mut self, id: window::Id, activity: Id) {
        if let Some(runtime) = self
            .outputs
            .iter()
            .find(|entry| entry.bar == id)
            .and_then(|entry| entry.runtime.hub.as_ref())
        {
            runtime.borrow_mut().activities.focus = Some(activity);
        } else if let Some(presentation) = self.activities.flat.get_mut(&id) {
            presentation.focus = Some(activity);
        }
    }
    fn activity_list_bar(&self, id: window::Id) -> Option<window::Id> {
        let bar = self
            .menu
            .as_ref()
            .filter(|menu| menu.id == id && menu.kind == status_ui::Menu::Activities)
            .map(|menu| menu.anchor.parent)
            .unwrap_or(id);
        let is_popup = self
            .menu
            .as_ref()
            .is_some_and(|menu| menu.id == id && menu.kind == status_ui::Menu::Activities);
        if !is_popup
            && self.hub_mode(bar)
                != Some(ferese_panel::morph::Mode::Expanded(
                    ferese_panel::morph::Content::Activities,
                ))
        {
            return None;
        }
        Some(bar)
    }
    pub(crate) fn activity_focus_query(
        &self,
        id: window::Id,
    ) -> Option<impl cosmic::iced::advanced::widget::Operation<Option<Id>> + use<>> {
        let bar = self.activity_list_bar(id)?;
        let hub = self.outputs.iter().find(|entry| entry.bar == bar)?.runtime.hub.as_ref();
        let rows = if let Some(hub) = hub {
            hub.borrow().activities.rows.clone()
        } else {
            self.activities.flat.get(&bar)?.rows.clone()
        };
        Some(FocusedActivity {
            bar,
            rows,
            focused: None,
        })
    }
    pub fn query_activity_focus(
        &self,
        id: window::Id,
        key: Option<cosmic::iced::keyboard::key::Named>,
    ) -> Option<Task<Message>> {
        Some(
            cosmic::iced::advanced::widget::operate(self.activity_focus_query(id)?)
                .map(move |focused| cosmic::Action::App(Message::ActivityNavigation(id, focused, key))),
        )
    }
    pub fn activity_key(&self, id: window::Id, key: cosmic::iced::keyboard::key::Named) -> Option<Task<Message>> {
        use cosmic::iced::keyboard::key::Named;
        if !matches!(key, Named::ArrowUp | Named::ArrowDown | Named::Home | Named::End) {
            return None;
        }
        self.query_activity_focus(id, Some(key))
    }
    pub fn activity_navigation(
        &mut self,
        id: window::Id,
        focused: Option<Id>,
        key: Option<cosmic::iced::keyboard::key::Named>,
    ) -> Option<Task<Message>> {
        let bar = self.activity_list_bar(id)?;
        let hub = self.outputs.iter().find(|entry| entry.bar == bar)?.runtime.hub.as_ref();
        let mut runtime = hub.map(|hub| hub.borrow_mut());
        let presentation = if let Some(runtime) = &mut runtime {
            &mut runtime.activities
        } else {
            self.activities.flat.get_mut(&bar)?
        };
        presentation.focus = focused
            .filter(|id| presentation.rows.contains(id))
            .or_else(|| presentation.focus.clone());
        let Some(key) = key else {
            return Some(Task::none());
        };
        use cosmic::iced::keyboard::key::Named;
        let index = presentation
            .focus
            .as_ref()
            .and_then(|id| presentation.rows.iter().position(|row| row == id))
            .unwrap_or(0);
        let next = match key {
            Named::ArrowUp => index.saturating_sub(1),
            Named::ArrowDown => (index + 1).min(presentation.rows.len().saturating_sub(1)),
            Named::Home => 0,
            _ => presentation.rows.len().saturating_sub(1),
        };
        presentation.focus = presentation.rows.get(next).cloned();
        Some(presentation.focus.as_ref().map_or_else(Task::none, |focus| {
            cosmic::widget::button::focus(activity_widget_id(bar, focus))
        }))
    }
    pub fn activity_result_tint(&self, id: window::Id) -> Option<(cosmic::iced::Color, f32)> {
        let runtime = self
            .outputs
            .iter()
            .find(|output| output.bar == id)?
            .runtime
            .hub
            .as_ref()?
            .borrow();
        let palette = self.config.theme.palette();
        runtime.activities.selection.visible.iter().find_map(|identity| {
            let entry = self
                .activities
                .snapshot
                .activities
                .iter()
                .find(|entry| &entry.id == identity)?;
            let alpha = runtime.activities.tints.get(identity)?.current as f32;
            (alpha > 0.).then_some((
                if entry.state == State::Failed {
                    palette.error
                } else {
                    palette.accent
                },
                alpha.clamp(0., 1.) * 0.4,
            ))
        })
    }
    pub fn activity_compact(
        &self,
        id: window::Id,
        opacity: f32,
        intrinsic: bool,
    ) -> Element<'_, cosmic::Action<Message>> {
        let palette = self.config.theme.palette();
        let runtime = self
            .outputs
            .iter()
            .find(|entry| entry.bar == id)
            .and_then(|entry| entry.runtime.hub.as_ref())
            .expect("activity hub")
            .borrow();
        let presentation = &runtime.activities;
        let mut strip = row::with_capacity(5)
            .spacing(compact::ITEM_GAP)
            .align_y(Alignment::Center);
        let mut descriptions = Vec::with_capacity(2);
        for (index, identity) in presentation.selection.visible.iter().enumerate() {
            let Some(activity) = self
                .activities
                .snapshot
                .activities
                .iter()
                .chain(self.activities.native.iter())
                .find(|entry| &entry.id == identity)
            else {
                continue;
            };
            descriptions.push(format!("{}, {}", activity.data.title, label(activity)));
            if index == 1 {
                let alpha = presentation
                    .divider
                    .as_ref()
                    .map_or(1., |value| value.current as f32)
                    .clamp(0., 1.);
                let divider_color = palette.text.scale_alpha(opacity * alpha * 0.25);
                strip = strip.push(container(cosmic::iced::widget::Space::new().width(1).height(18)).class(
                    theme::Container::custom(move |_| container::Style {
                        background: Some(divider_color.into()),
                        ..Default::default()
                    }),
                ));
            }
            let semantic = if activity.state == State::Failed {
                palette.error
            } else if activity.state == State::Done {
                palette.accent
            } else {
                palette.text
            };
            let blend = presentation
                .tints
                .get(identity)
                .map_or(0., |value| value.current as f32)
                .clamp(0., 1.);
            let tint = cosmic::iced::Color::from_rgba(
                palette.text.r + (semantic.r - palette.text.r) * blend,
                palette.text.g + (semantic.g - palette.text.g) * blend,
                palette.text.b + (semantic.b - palette.text.b) * blend,
                palette.text.a,
            );
            let title = self
                .render
                .text(activity.data.title.clone())
                .size(12)
                .width(Length::Fill)
                .line_height(cosmic::iced::widget::text::LineHeight::Relative(1.1))
                .class(theme::Text::Color(palette.text.scale_alpha(opacity)))
                .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                    cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
                ));
            let mut text = column![title].spacing(0).width(Length::Fill);
            if let Some(detail) = detail(activity) {
                text = text.push(
                    self.render
                        .text(detail)
                        .size(10)
                        .width(Length::Fill)
                        .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                            cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
                        ))
                        .line_height(cosmic::iced::widget::text::LineHeight::Relative(1.1))
                        .class(theme::Text::Color(palette.text.scale_alpha(opacity * 0.7))),
                );
            }
            let text = container(text)
                .center_y(26)
                .width(presentation.compact.text_width(identity));
            let item = row![
                self.activity_icon(activity, tint.scale_alpha(opacity), !intrinsic),
                text
            ]
            .spacing(compact::TEXT_GAP)
            .align_y(Alignment::Center)
            .width(Length::Shrink);
            strip = strip.push(item);
        }
        if presentation.selection.hidden > 0 {
            strip = strip.push(
                self.render
                    .text(format!("+{}", presentation.selection.hidden))
                    .size(11)
                    .class(theme::Text::Color(palette.text.scale_alpha(opacity))),
            );
        }
        let mut controls = row::with_capacity(2).spacing(10).align_y(Alignment::Center);
        if self.hub_workspace_width(id) > 0. {
            controls = controls.push(self.hub_workspaces(id, opacity));
        }
        let appearance = button::Style {
            text_color: Some(palette.text.scale_alpha(opacity)),
            icon_color: Some(palette.text.scale_alpha(opacity)),
            ..Default::default()
        };
        controls = controls.push(
            button::custom(container(strip).center_y(Length::Fill))
                .name(format!(
                    "Activities: {}. Open the activity list",
                    descriptions.join("; ")
                ))
                .padding(0)
                .width(Length::Fill)
                .height(Length::Fill)
                .class(theme::Button::Custom {
                    active: Box::new(move |_, _| appearance),
                    hovered: Box::new(move |_, _| appearance),
                    pressed: Box::new(move |_, _| appearance),
                    disabled: Box::new(move |_| appearance),
                })
                .on_press(cosmic::Action::App(Message::HubOpen(id, status_ui::Menu::Activities))),
        );
        let budget = presentation.compact.width
            + if self.hub_workspace_width(id) > 0. {
                self.hub_workspace_width(id) as f32 + 10.
            } else {
                0.
            };
        container(controls)
            .padding([0., compact::PADDING])
            .height(Length::Fill)
            .width(if intrinsic { Length::Fixed(budget) } else { Length::Fill })
            .into()
    }
    pub fn activity_list(&self, id: window::Id, opacity: f32) -> Element<'_, cosmic::Action<Message>> {
        let palette = self.config.theme.palette();
        let name = self.output_for_bar(id).map(|output| output.name.as_str()).unwrap_or("");
        let entries = list_entries(&self.activities.snapshot, &self.activities.native, now_ms(), name);
        let focus = self
            .outputs
            .iter()
            .find(|entry| entry.bar == id)
            .and_then(|entry| entry.runtime.hub.as_ref())
            .and_then(|runtime| runtime.borrow().activities.focus.clone())
            .or_else(|| {
                self.activities
                    .flat
                    .get(&id)
                    .and_then(|presentation| presentation.focus.clone())
            });
        let mut rows = Vec::with_capacity(entries.len());
        if entries.is_empty() {
            return self.render.text("No active tasks").size(13).into();
        }
        for activity in entries {
            let mut text = column![
                self.render
                    .text(activity.data.title.clone())
                    .size(13)
                    .line_height(cosmic::iced::widget::text::LineHeight::Relative(1.1))
                    .width(Length::Fill)
                    .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                        cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
                    ))
            ]
            .spacing(0)
            .width(Length::Fill);
            if let Some(detail) = detail(activity) {
                text = text.push(
                    self.render
                        .text(detail)
                        .size(12)
                        .line_height(cosmic::iced::widget::text::LineHeight::Relative(1.1))
                        .width(Length::Fill)
                        .ellipsize(cosmic::iced::widget::text::Ellipsize::End(
                            cosmic::iced::advanced::text::EllipsizeHeightLimit::Lines(1),
                        ))
                        .class(theme::Text::Color(palette.text.scale_alpha(opacity * 0.7))),
                );
            }
            // Keep the row stable when progress becomes a result or a text phase.
            let text = container(text).center_y(28).width(Length::Fill);
            let mut body = column![
                row![
                    self.activity_icon(activity, palette.text.scale_alpha(opacity), false),
                    text
                ]
                .spacing(8)
                .align_y(Alignment::Center)
            ]
            .spacing(4)
            .width(Length::Fill);
            match &activity.data.template {
                Template::Media { artist, playing } => {
                    body = body.push(
                        self.render
                            .text(format!("{artist} · {}", if *playing { "Playing" } else { "Paused" }))
                            .size(12),
                    );
                }
                Template::List { rows } => {
                    for text in rows {
                        body = body.push(self.render.text(text.clone()).size(12));
                    }
                }
                _ => {}
            }
            if activity.data.source == ferese_activities::Source::Media && activity.id.session == 0 {
                body = body.push(
                    button::custom(icons::tinted(icons::MEDIA_PLAY, 16, palette.text))
                        .name("Play or pause")
                        .on_press(cosmic::Action::App(Message::MediaAction(
                            self.media.action("play-pause"),
                        ))),
                );
            }
            let selected = focus.as_ref() == Some(&activity.id);
            let radius = self.render.radius(10.);
            let appearance = move |strength: f32, focused: bool| button::Style {
                background: Some(palette.text.scale_alpha(strength * opacity).into()),
                text_color: Some(palette.text.scale_alpha(opacity)),
                icon_color: Some(palette.text.scale_alpha(opacity)),
                shape: Some(cosmic::iced::border::Shape::Continuous),
                border_radius: radius.into(),
                outline_width: if focused { 1. } else { 0. },
                outline_color: palette.accent.scale_alpha(opacity),
                ..Default::default()
            };
            rows.push(
                row![
                    button::custom(body)
                        .padding([6, 8])
                        .id(activity_widget_id(id, &activity.id))
                        .name(format!(
                            "{}, {}{}",
                            activity.data.title,
                            label(activity),
                            if self.activity_window(&activity.id).is_some() {
                                ". Open terminal"
                            } else {
                                ""
                            }
                        ))
                        .selected(selected)
                        .class(theme::Button::Custom {
                            active: Box::new(move |focused, _| appearance(0.045, focused)),
                            hovered: Box::new(move |focused, _| appearance(0.075, focused)),
                            pressed: Box::new(move |focused, _| appearance(0.10, focused)),
                            disabled: Box::new(move |_| appearance(0.025, false)),
                        })
                        .on_press(cosmic::Action::App(Message::ActivityActivate(id, activity.id.clone())))
                        .width(Length::Fill),
                ]
                .width(Length::Fill)
                .into(),
            );
        }
        // Keep unnamed row roots: the toolkit's column does not move directly
        // named child trees when their order changes. Inner button IDs restore
        // focus through the normal named-state reconciliation on each rebuild.
        column::with_children(rows).spacing(4).width(Length::Fill).into()
    }
}
