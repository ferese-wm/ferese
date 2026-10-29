use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub type Time = u64;
const BOOST_MS: u64 = 5_000;
pub const MAX_TEXT: usize = 256;
pub const MAX_LEASE_MS: u64 = 300_000;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Owner {
    pub pid: u32,
    pub birth: u64,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Id {
    pub owner: Owner,
    pub local: String,
    pub generation: u64,
    pub session: u64,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    content = "value",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum Value {
    Indeterminate,
    Progress(f32),
    Text(String),
    Countdown { remaining_ms: u64, sampled_at: Time },
    PausedCountdown { remaining_ms: u64 },
}

impl Value {
    #[must_use]
    pub fn remaining_ms(&self, now: Time) -> Option<u64> {
        match self {
            Self::Countdown {
                remaining_ms,
                sampled_at,
            } => Some(remaining_ms.saturating_sub(now.saturating_sub(*sampled_at))),
            Self::PausedCountdown { remaining_ms } => Some(*remaining_ms),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum Template {
    Progress,
    Timer,
    Media { artist: String, playing: bool },
    List { rows: Vec<String> },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    Publisher,
    Command,
    Media,
    Capture,
    Download,
    Timer,
    Notification,
    Process,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Data {
    pub source: Source,
    pub output: Option<String>,
    pub priority: u8,
    pub icon: String,
    pub title: String,
    pub value: Value,
    pub template: Template,
}

impl Data {
    pub fn validate(&mut self, now: Time) -> Result<(), &'static str> {
        text(&self.title)?;
        if self
            .output
            .as_ref()
            .is_some_and(|output| text(output).is_err())
        {
            return Err("invalid output name");
        }
        if self.icon.len() > 64
            || !self
                .icon
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("icon must be a symbolic name");
        }
        if !matches!(
            self.icon.as_str(),
            "hammer"
                | "timer"
                | "download"
                | "media"
                | "microphone"
                | "camera"
                | "call"
                | "notification"
                | "ferese"
        ) {
            self.icon = "ferese".into();
        }
        match &mut self.value {
            Value::Progress(progress)
                if !progress.is_finite() || !(0.0..=1.0).contains(progress) =>
            {
                return Err("progress must be finite and between zero and one");
            }
            Value::Text(value) => text(value)?,
            Value::PausedCountdown { remaining_ms } if *remaining_ms > 7 * 24 * 60 * 60 * 1_000 => {
                return Err("countdown exceeds seven days");
            }
            Value::Countdown {
                remaining_ms,
                sampled_at,
            } => {
                if *remaining_ms > 7 * 24 * 60 * 60 * 1_000 {
                    return Err("countdown exceeds seven days");
                }
                *sampled_at = now;
            }
            _ => {}
        }
        match &self.template {
            Template::Timer
                if !matches!(
                    self.value,
                    Value::Countdown { .. } | Value::PausedCountdown { .. }
                ) =>
            {
                return Err("timer requires a countdown");
            }
            Template::Progress
                if matches!(
                    self.value,
                    Value::Countdown { .. } | Value::PausedCountdown { .. }
                ) =>
            {
                return Err("countdown requires timer template");
            }
            Template::Media { artist, .. } => {
                text(artist)?;
                if !matches!(
                    self.value,
                    Value::Text(_) | Value::Indeterminate | Value::Progress(_)
                ) {
                    return Err("invalid media value");
                }
            }
            Template::List { rows } => {
                if rows.len() > 8 {
                    return Err("list exceeds eight rows");
                }
                for row in rows {
                    text(row)?;
                }
                if !matches!(self.value, Value::Text(_) | Value::Indeterminate) {
                    return Err("invalid list value");
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn text(value: &str) -> Result<(), &'static str> {
    if value.len() > MAX_TEXT || value.chars().any(char::is_control) {
        Err("text exceeds its budget or contains control characters")
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub max_visible: usize,
    pub show_badge: bool,
    pub max_total: usize,
    pub owner_ceiling: u8,
    pub start_delay_ms: u64,
    pub result_flash_ms: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_visible: 2,
            show_badge: true,
            max_total: 128,
            owner_ceiling: 160,
            start_delay_ms: 300,
            result_flash_ms: 1_500,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Activity {
    pub id: Id,
    pub data: Data,
    pub state: State,
    pub created_at: Time,
    pub eligible_at: Time,
    pub refreshed_at: Time,
    pub lease_deadline: Option<Time>,
    pub result_deadline: Option<Time>,
    pub result_message: Option<String>,
    pub flash_on_fail: bool,
    pub result_flash_ms: u64,
    /// A presentation group only; every invocation retains its own lifetime.
    pub group: Option<String>,
}

impl Activity {
    #[must_use]
    pub fn eligible(&self, now: Time, dnd: bool) -> bool {
        now >= self.eligible_at
            && match self.state {
                State::Running => self.lease_deadline.is_none_or(|deadline| now < deadline),
                State::Done | State::Failed => {
                    !dnd && self.result_deadline.is_some_and(|deadline| now < deadline)
                }
                State::Cancelled => false,
            }
    }

    #[must_use]
    pub fn score(&self, now: Time, ceiling: u8) -> u16 {
        u16::from(self.data.priority.min(ceiling))
            + if now < self.eligible_at.saturating_add(BOOST_MS) {
                30
            } else {
                0
            }
            + match self.state {
                State::Failed => 100,
                State::Done => 40,
                _ => 0,
            }
    }
}

#[derive(Debug)]
pub struct Store {
    pub limits: Limits,
    session: u64,
    generation: u64,
    entries: BTreeMap<Id, Activity>,
}

impl Store {
    #[must_use]
    pub fn new(session: u64) -> Self {
        Self {
            limits: Limits::default(),
            session,
            generation: 0,
            entries: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn session(&self) -> u64 {
        self.session
    }

    pub fn entries(&self) -> impl Iterator<Item = &Activity> {
        self.entries.values()
    }

    #[must_use]
    pub fn resolve(&self, owner: Owner, local: &str) -> Option<&Id> {
        self.entries
            .keys()
            .find(|id| id.owner == owner && id.local == local)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn start(
        &mut self,
        owner: Owner,
        local: String,
        mut data: Data,
        now: Time,
        min_duration_ms: u64,
        lease_ms: Option<u64>,
        monitored: bool,
        flash_on_fail: bool,
        group: Option<String>,
    ) -> Result<Id, &'static str> {
        self.advance(now);
        text(&local)?;
        if local.is_empty() {
            return Err("activity ID must not be empty");
        }
        data.validate(now)?;
        validate_lease(lease_ms, monitored)?;
        if self.resolve(owner, &local).is_some() {
            return Err("duplicate activity ID");
        }
        if self.entries.len() >= self.limits.max_total {
            return Err("session activity quota exceeded");
        }
        if self.entries.keys().filter(|id| id.owner == owner).count() >= 3 {
            return Err("owner activity quota exceeded");
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or("generation exhausted")?;
        let id = Id {
            owner,
            local,
            generation: self.generation,
            session: self.session,
        };
        self.entries.insert(
            id.clone(),
            Activity {
                id: id.clone(),
                data,
                state: State::Running,
                created_at: now,
                eligible_at: now.saturating_add(self.limits.start_delay_ms.max(min_duration_ms)),
                refreshed_at: now,
                lease_deadline: lease_ms.map(|ms| now.saturating_add(ms)),
                result_deadline: None,
                result_message: None,
                flash_on_fail,
                result_flash_ms: self.limits.result_flash_ms,
                group,
            },
        );
        Ok(id)
    }

    fn entry(&mut self, owner: Owner, id: &Id) -> Result<&mut Activity, &'static str> {
        if id.session != self.session {
            return Err("stale activity session; register again");
        }
        if id.owner != owner {
            return Err("activity owner rejected");
        }
        self.entries.get_mut(id).ok_or("stale activity generation")
    }

    pub fn update(
        &mut self,
        owner: Owner,
        id: &Id,
        value: Option<Value>,
        lease_ms: Option<u64>,
        now: Time,
    ) -> Result<(), &'static str> {
        self.advance(now);
        validate_lease(lease_ms, true)?;
        let entry = self.entry(owner, id)?;
        if entry.state != State::Running {
            return Err("activity already completed");
        }
        if let Some(value) = value {
            let mut candidate = entry.data.clone();
            candidate.value = value;
            candidate.validate(now)?;
            entry.data = candidate;
        }
        if let Some(ms) = lease_ms {
            validate_lease(Some(ms), true)?;
            entry.lease_deadline = Some(now.saturating_add(ms));
        } else if let Some(deadline) = entry.lease_deadline {
            entry.lease_deadline =
                Some(now.saturating_add(deadline.saturating_sub(entry.refreshed_at)));
        }
        entry.refreshed_at = now;
        Ok(())
    }

    pub fn finish(
        &mut self,
        owner: Owner,
        id: &Id,
        state: State,
        message: Option<String>,
        now: Time,
    ) -> Result<(), &'static str> {
        self.advance(now);
        if state == State::Running {
            return Err("finish requires a terminal result");
        }
        if let Some(message) = &message {
            text(message)?;
        }
        let entry = self.entry(owner, id)?;
        if entry.state != State::Running {
            return if entry.state == state {
                Ok(())
            } else {
                Err("terminal result already accepted")
            };
        }
        if state == State::Cancelled
            || now < entry.eligible_at
            || (state == State::Failed && !entry.flash_on_fail)
        {
            self.entries.remove(id);
            return Ok(());
        }
        entry.state = state;
        if let Some(message) = message {
            entry.result_message = Some(message);
        }
        entry.result_deadline = Some(now.saturating_add(entry.result_flash_ms));
        Ok(())
    }

    pub fn owner_exit(&mut self, owner: Owner) {
        self.entries
            .retain(|id, entry| id.owner != owner || entry.state != State::Running);
    }

    pub fn remove_output(&mut self, output: &str, fallback: Option<&str>) {
        let entry_iter = self
            .entries
            .values_mut()
            .filter(|entry| entry.data.output.as_deref() == Some(output));
        for entry in entry_iter {
            entry.data.output = fallback.map(str::to_owned);
        }
    }

    pub fn attach_output(&mut self, output: &str) -> bool {
        let mut changed = false;
        let entry_iter = self
            .entries
            .values_mut()
            .filter(|entry| entry.data.output.is_none());
        for entry in entry_iter {
            entry.data.output = Some(output.into());
            changed = true;
        }
        changed
    }
    pub fn advance(&mut self, now: Time) -> bool {
        let before = self.entries.len();
        self.entries.retain(|_, entry| match entry.state {
            State::Running => entry.lease_deadline.is_none_or(|deadline| now < deadline),
            _ => entry.result_deadline.is_some_and(|deadline| now < deadline),
        });
        before != self.entries.len()
    }

    #[must_use]
    pub fn next_deadline(&self, now: Time) -> Option<Time> {
        self.entries
            .values()
            .flat_map(|entry| {
                [
                    Some(entry.eligible_at),
                    Some(entry.eligible_at.saturating_add(BOOST_MS)),
                    entry.lease_deadline,
                    entry.result_deadline,
                    entry.result_deadline.map(|deadline| {
                        deadline
                            .saturating_sub(entry.result_flash_ms)
                            .saturating_add(400)
                    }),
                ]
            })
            .flatten()
            .filter(|deadline| *deadline > now)
            .min()
    }
}

fn validate_lease(lease: Option<u64>, monitored: bool) -> Result<(), &'static str> {
    match lease {
        Some(0) => Err("lease must be positive"),
        Some(ms) if ms > MAX_LEASE_MS => Err("lease exceeds five minutes"),
        None if !monitored => Err("unmonitored owner requires a lease"),
        _ => Ok(()),
    }
}

/// Recording/battery occupy a separate host-only class. Notifications always
/// follow every other ordinary source, even when a result boost is active.
pub fn ranked<'a>(
    entries: impl IntoIterator<Item = &'a Activity>,
    now: Time,
    limits: Limits,
    output: &str,
    dnd: bool,
) -> Vec<&'a Activity> {
    let mut entries: Vec<_> = entries
        .into_iter()
        .filter(|entry| entry.eligible(now, dnd) && entry.data.output.as_deref() == Some(output))
        .collect();
    entries.sort_by(|a, b| {
        (a.data.source == Source::Notification)
            .cmp(&(b.data.source == Source::Notification))
            .then_with(|| {
                b.score(now, limits.owner_ceiling)
                    .cmp(&a.score(now, limits.owner_ceiling))
            })
            .then_with(|| a.eligible_at.cmp(&b.eligible_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    entries
}

#[derive(Clone, Debug, Default)]
pub struct Selection {
    pinned: Option<Vec<Id>>,
    pub visible: Vec<Id>,
    pub hidden: usize,
    policy: Option<(bool, usize, usize)>,
}

impl Selection {
    pub fn enter(&mut self) {
        self.pinned = Some(self.visible.clone());
    }
    pub fn leave(&mut self) {
        self.pinned = None;
    }
    pub fn override_pin(&mut self) {
        self.pinned = None;
    }
    pub fn select(
        &mut self,
        ranked: &[&Activity],
        limits: Limits,
        dnd: bool,
        protected_slots: usize,
    ) {
        let policy = (dnd, protected_slots, limits.max_visible);
        if self.policy != Some(policy) {
            self.override_pin();
            self.policy = Some(policy);
        }
        let count = limits
            .max_visible
            .clamp(1, 2)
            .saturating_sub(protected_slots)
            .min(if dnd { 1 } else { 2 });
        let eligible: Vec<_> = ranked
            .iter()
            .filter(|entry| !dnd || entry.state == State::Running)
            .collect();
        let mut groups = std::collections::BTreeSet::new();
        let mut ids: Vec<_> = eligible
            .iter()
            .filter(|entry| {
                let Some(group) = &entry.group else {
                    return true;
                };
                if entry.state != State::Running
                    && eligible.iter().any(|other| {
                        other.id.owner == entry.id.owner
                            && other.group.as_ref() == Some(group)
                            && other.state == State::Running
                    })
                {
                    return false;
                }
                groups.insert((entry.id.owner, group.as_str()))
            })
            .map(|entry| &entry.id)
            .collect();
        if let Some(pinned) = &mut self.pinned {
            pinned.retain(|id| ids.contains(&id));
            ids.clear();
            ids.extend(pinned.iter());
        }
        let selected = &ids[..count.min(ids.len())];
        if !self.visible.iter().eq(selected.iter().copied()) {
            self.visible.clear();
            self.visible.extend(selected.iter().copied().cloned());
        }
        self.hidden = if dnd || !limits.show_badge {
            0
        } else {
            eligible.len().saturating_sub(self.visible.len())
        };
    }
}

/// Focus survives reordered/updated rows by identity; removal chooses the next
/// surviving row at the former position.
pub fn reconcile_focus(focus: &mut Option<Id>, previous: &[Id], current: &[Id]) {
    if focus.as_ref().is_some_and(|id| current.contains(id)) {
        return;
    }
    let index = focus
        .as_ref()
        .and_then(|id| previous.iter().position(|old| old == id))
        .unwrap_or(0);
    *focus = current
        .get(index.min(current.len().saturating_sub(1)))
        .cloned();
}
