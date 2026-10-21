//! Activity schema uses the session's existing versioned framing and socket.
use ferese_activities::{Activity, Data, Id, Limits, Value};
use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;

pub fn now_ms() -> u64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: time is a correctly sized writable timespec. CLOCK_BOOTTIME
    // includes suspend, unlike the renderer's animation clock.
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &raw mut time) } != 0 {
        return 0;
    }
    (time.tv_sec.max(0) as u64)
        .saturating_mul(1_000)
        .saturating_add(time.tv_nsec.max(0) as u64 / 1_000_000)
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum OwnerScope {
    #[default]
    Parent,
    Publisher,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Publish {
    Start {
        local: String,
        data: Data,
        lease_ms: Option<u64>,
    },
    Update {
        local: String,
        handle: Option<Id>,
        value: Option<Value>,
        lease_ms: Option<u64>,
    },
    End {
        local: String,
        handle: Option<Id>,
        result: ferese_activities::State,
        message: Option<String>,
    },
    Cancel {
        local: String,
        handle: Option<Id>,
    },
    /// Opt-in rule matching. Raw metadata is discarded before dispatch.
    Hook {
        local: String,
        command: String,
        cwd: String,
    },
    Run {
        local: String,
        title: Option<String>,
        command: String,
        cwd: String,
    },
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u32,
    #[serde(default)]
    pub owner: OwnerScope,
    pub publish: Publish,
}
/// Assigned by the compositor, never supplied by publishers.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityWindow {
    pub activity: Id,
    pub window: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub version: u32,
    pub revision: u64,
    pub now_ms: u64,
    pub limits: Limits,
    pub activities: Vec<Activity>,
    #[serde(default)]
    pub windows: Vec<ActivityWindow>,
    pub pomodoro: Option<Pomodoro>,
}
impl Default for Snapshot {
    fn default() -> Self {
        Self {
            version: VERSION,
            revision: 0,
            now_ms: now_ms(),
            limits: Limits::default(),
            activities: vec![],
            windows: vec![],
            pomodoro: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PomodoroAction {
    Start,
    Pause,
    Resume,
    Next,
    Stop,
    Reset,
    Status,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PomodoroRequest {
    pub version: u32,
    pub action: PomodoroAction,
    /// Popup controls carry the displayed generation, so a delayed click cannot
    /// affect a replacement session. CLI commands may address the current one.
    pub session: Option<Id>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Pomodoro {
    pub id: Id,
    pub timer: ferese_activities::pomodoro::Session,
}
